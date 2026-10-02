// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! Bill totals, tax, and settlement — the arithmetic ADR-0028 pins down.
//!
//! Everything here is a pure function over `pos-proto` value types: no clock, no I/O, no state.
//! Given the lines' pre-tax bases per tax class, the bill-level reductions, the service charge, the
//! fee rules and the store's rate table, [`assemble`] produces a [`BillTotals`] whose components
//! reconcile to the total exactly; given a total and a set of payments, [`settle`] proves the
//! settlement invariant.
//!
//! # The three things ADR-0028 fixes, made mechanical
//!
//! - **Tax rounds per tax-class subtotal**, once, not per line and not per bill. [`assemble`] groups
//!   by class, applies the channel-keyed rate to each class's discounted base, rounds that once, and
//!   sums — and [`BillTotals::tax_lines`] is the per-class breakdown a VAT invoice prints, which by
//!   construction sums to [`BillTotals::tax_total`].
//! - **Cash rounding is an explicit line.** [`BillTotals::rounding_adjustment`] is the difference
//!   between the rounded and unrounded totals, so the printed lines reconcile to the printed total.
//! - **Tips are not part of the total.** [`settle`] takes them separately and they appear only in
//!   the change identity, never in `total_due`.
//!
//! # Comps versus discounts
//!
//! A discount reduces what is owed and what is taxed. A comp (`pos-spec.md` §5) also removes the
//! amount from what the guest pays, but it is *given away* — it still consumes inventory and is
//! recorded as cost. Here both reduce the taxable base and the total; the cost side of a comp is an
//! inventory concern, tracked elsewhere. Keeping them distinct in [`BillTotals`] is what lets
//! accounting and fraud analysis treat them differently, as the specification requires.
//!
//! # Fees
//!
//! A fee is a published rule ([ADR-0159](../../../docs/adr/0159-a-fee-is-configuration.md)), and
//! [`assemble`] charges every rule that applies to the bill as one [`FeeLine`]. A percentage is
//! taken of the lines it counts, after their share of the bill's discount and comps, and never of
//! another fee, so fees do not compound. Each fee rounds once, by the bill's rounding mode. Its
//! taxed parts join their classes' bases before each class's tax is rounded, so tax still rounds
//! once per class (ADR-0028); each part then carries its share of that class's tax.
//! [`BillTotals::service_charge`] carries the sum of every fee, so the readers of that one figure
//! stay right (ADR-0159 decision 4).
//!
//! A fee waived on a bill (ADR-0159 decision 5) is left out of it: [`assemble`] charges and taxes
//! the bill as though the rule were not there, and the rule's `fee_id` is what keeps it waived on
//! every part of a split and every merge the bill joins ([`MergedFees::waived_fee_ids`]).

use pos_proto::fees::{FeeCode, FeeKind, FeeTax, FrozenFee};
use pos_proto::locale::{TaxComponent, TaxRateTable};
use pos_proto::money::{Money, MoneyError, Ratio, Rounding, div_round};
use pos_proto::quantity::Quantity;
use pos_proto::text::DisplayName;
use pos_proto::{CurrencyCode, FeeId, MenuItemId, PaymentMethod, SalesChannel, TaxClassId};

use crate::error::DomainError;

/// One line of tax on the bill: a class, the base it was charged on, and the tax itself.
///
/// The unit a VAT invoice prints. `tax_class_id` and `taxable_base` are kept beside `tax` so the
/// printed line is self-explaining and the reconciliation is visible rather than asserted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaxLine {
    /// The class this tax was charged at.
    pub tax_class_id: TaxClassId,
    /// The base the rate was applied to, after discounts and comps, plus the service charge when it
    /// is taxable and belongs to this class, plus every fee's part taxed at this class.
    pub taxable_base: Money,
    /// The rate applied, in basis points, captured so the invoice shows it.
    pub rate_basis_points: u32,
    /// The tax, rounded once for this class.
    pub tax: Money,
    /// How that tax breaks out, when the country requires the invoice to say
    /// ([ADR-0104](../../../docs/adr/0104-multi-component-and-inclusive-tax.md)). Empty everywhere
    /// the rate prints as one number, which is most of the world.
    ///
    /// The parts are allocated out of `tax` after it is rounded, so they sum to it exactly. Rounding
    /// each part independently would let the breakdown miss the total it claims to explain.
    pub components: Vec<TaxComponentLine>,
}

/// One named part of a tax line, with the money that part accounts for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaxComponentLine {
    /// What the invoice calls it — `CGST`, `SGST`, `IGST`.
    pub name: String,
    /// This part's rate, captured so the invoice can print it beside the amount.
    pub rate_basis_points: u32,
    /// This part's share of the line's tax. The shares sum exactly to [`TaxLine::tax`].
    pub tax: Money,
}

/// Splits a rounded tax amount across its named components, in proportion to their rates.
///
/// Allocation rather than per-component multiplication, for the reason on [`TaxLine::components`]:
/// the parts have to sum to the whole, and `Money::allocate` is the one primitive in this crate that
/// guarantees it (the residual lands on the last part, as it does for the bill-level discount).
///
/// An empty component list yields no lines — "no breakdown", which is not the same as a breakdown of
/// zero parts summing to the tax.
fn split_components(
    tax: Money,
    components: &[TaxComponent],
) -> Result<Vec<TaxComponentLine>, DomainError> {
    if components.is_empty() {
        return Ok(Vec::new());
    }
    let weights: Vec<i64> = components
        .iter()
        .map(|component| i64::from(component.rate.basis_points()))
        .collect();
    // Every component at a zero rate would make the weights unallocatable. That is a table a country
    // should not have published, and `TaxRateTable::unbalanced_rows` catches it upstream; here it
    // degrades to no breakdown rather than failing a sale over a printing concern.
    if weights.iter().all(|weight| *weight == 0) {
        return Ok(Vec::new());
    }
    let shares = tax.allocate(&weights)?;
    Ok(components
        .iter()
        .zip(shares)
        .map(|(component, share)| TaxComponentLine {
            name: component.name.clone(),
            rate_basis_points: component.rate.basis_points(),
            tax: share,
        })
        .collect())
}

/// The pre-tax base for one tax class, before bill-level reductions.
///
/// The caller groups the bill's lines by class and sums each group's net (line-level promotions
/// already applied at add time, `pos-spec.md` §14.2). Bill-level discount and comps are applied
/// here, proportionally, so this is the *input*, not the taxable base.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ClassBase {
    /// The class.
    pub tax_class_id: TaxClassId,
    /// The sum of that class's line nets, before bill-level reductions.
    pub amount: Money,
}

/// One sold line, as a fee rule matches it (ADR-0159 decision 2).
///
/// The caller passes the same lines it summed into its [`ClassBase`]s, so the two describe one
/// bill: a rule matches a line by its item, and counts its quantity and its net.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BillLine {
    /// The item, which a rule's item scope counts or leaves out.
    pub menu_item_id: MenuItemId,
    /// How many were sold, which a fee per unit charges for. Fractional for a half-and-half line.
    pub quantity: Quantity,
    /// What the line comes to, as its class's [`ClassBase`] sums it: before the bill-level discount
    /// and comps, which fall on every line in proportion to its net.
    pub net: Money,
    /// The class the line is taxed at.
    pub tax_class_id: TaxClassId,
}

/// The part of a fee taxed at one class, and that part's tax.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FeeClassShare {
    /// The class.
    pub tax_class_id: TaxClassId,
    /// The part of the fee that joins this class's taxable base.
    pub amount: Money,
    /// This part's share of the class's tax, in proportion to its part of the class's base. The
    /// class's tax rounds once on its whole base (ADR-0028), and its parts share it out exactly.
    pub tax: Money,
}

/// One fee rule applied to a bill: what it charged, and where that charge is taxed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FeeLine {
    /// The rule that charged it.
    pub fee_id: FeeId,
    /// The rule's code, what a report groups fees by.
    pub code: FeeCode,
    /// The rule's own name, as the bill froze it: what the bill prints the fee under.
    pub display_name: DisplayName,
    /// What it charged, rounded once to the minor unit by the bill's rounding mode.
    pub amount: Money,
    /// The parts of `amount` taxed at each class, which sum to it. There are none for a fee that is
    /// not taxed, one for a fee taxed at a named class, and for a fee that follows its lines one
    /// per class of the lines it counts, in proportion to their nets (their quantities, for a fee
    /// per unit). Each carries its share of its class's tax.
    pub class_shares: Vec<FeeClassShare>,
    /// The tax on the fee: the sum of its parts' taxes. Zero for a fee that is not taxed.
    pub tax: Money,
    /// Whether the rule lets staff waive the fee on one bill, as the bill froze it (ADR-0159
    /// decision 5), so a till can offer the act only where it is allowed.
    pub waivable: bool,
}

/// Everything needed to assemble a bill's totals.
#[derive(Debug, Clone)]
pub struct BillInput<'a> {
    /// The bill's currency. Every amount must match it.
    pub currency_code: CurrencyCode,
    /// Pre-discount net per tax class. Empty is an error: a bill has lines.
    pub class_bases: &'a [ClassBase],
    /// A bill-level discount, non-negative, allocated across classes in proportion to their bases.
    pub bill_discount: Money,
    /// Comped amount, non-negative, allocated the same way. Reduces the total and the taxable base;
    /// its cost is an inventory concern.
    pub comps: Money,
    /// The service charge, non-negative, applied after discounts and before tax.
    pub service_charge: Money,
    /// Whether the service charge is taxable — `store.tax.service_charge_taxable`, default true.
    pub service_charge_taxable: bool,
    /// The class the service charge is taxed at, when taxable. `None` means untaxed regardless.
    pub service_charge_tax_class: Option<TaxClassId>,
    /// The store's channel-keyed rate table.
    pub rates: &'a TaxRateTable,
    /// The channel this bill's order came in on, which selects the tax rate.
    pub sales_channel: SalesChannel,
    /// The cash-rounding increment in minor units (500 for VND rounding to the nearest 500), or
    /// `None` for no rounding. Applied to the grand total, materialised as an explicit adjustment.
    pub cash_rounding_increment: Option<i64>,
    /// The rounding mode for tax and for cash rounding.
    pub rounding_mode: Rounding,
    /// Whether the class bases already contain their tax — the store's `locale.prices_include_tax`
    /// ([ADR-0104](../../../docs/adr/0104-multi-component-and-inclusive-tax.md)).
    ///
    /// `false` is Vietnam and every exclusive-pricing market: tax is added on top of the base.
    /// `true` is Japan's 税込 and India's MRP: the base already contains the tax, so the tax is
    /// *extracted* from it and the guest's total does not move.
    pub prices_include_tax: bool,
    /// The fee rules in force for the bill, frozen, in the order their [`FeeLine`]s are reported:
    /// the rules `billing.bill.opened` recorded for an open bill, or for a bill not yet opened
    /// those `PublishedFees::in_force` gives for its channel (ADR-0159 decision 3). Freezing has
    /// already left out a paused rule and one for another channel. Empty, as the edge passes
    /// today, charges no fee and leaves every other figure as it was.
    ///
    /// A rule applies to nothing when [`FrozenFee::violations`] faults it (a token from a newer
    /// release that this one does not know), when it counts no line, or when it charges in another
    /// currency. So does a percentage whose base is in the other tax posture from
    /// [`Self::prices_include_tax`]: this release does not convert between the two (ADR-0104).
    pub fee_rules: &'a [FrozenFee],
    /// The fees waived on this bill (`billing.fee.waived`, ADR-0159 decision 5), by id. A rule
    /// listed here is left out: it charges nothing and taxes nothing, and every other figure is
    /// what it would be with the rule not frozen on the bill at all. Empty, as for every bill not
    /// yet opened, waives nothing.
    pub waived_fee_ids: &'a [FeeId],
    /// The bill's sold lines, which the rules match. Read only when a rule is left to charge, and
    /// then each class's lines must sum to its [`ClassBase`].
    pub lines: &'a [BillLine],
}

/// A bill's computed totals, every component reconciling to [`Self::total_due`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BillTotals {
    /// Sum of the class bases, before any bill-level reduction.
    pub subtotal: Money,
    /// The bill-level discount applied.
    pub discount_total: Money,
    /// The comped amount.
    pub comp_total: Money,
    /// The service charge: the input's service charge plus every fee in [`Self::fee_lines`], so the
    /// readers of this one figure see every charge (ADR-0159 decision 4).
    pub service_charge: Money,
    /// One line per fee rule that applied, in rule order.
    pub fee_lines: Vec<FeeLine>,
    /// Per-class tax, each rounded once. Sums to [`Self::tax_total`].
    pub tax_lines: Vec<TaxLine>,
    /// The total tax.
    pub tax_total: Money,
    /// The cash-rounding adjustment: rounded total minus unrounded total. Zero when no rounding.
    pub rounding_adjustment: Money,
    /// What the guest owes: `subtotal − discount − comps + service_charge + tax + rounding`.
    pub total_due: Money,
}

/// Allocates a non-negative amount across the class bases in proportion to each base.
///
/// Returns one share per class, summing exactly to `amount` (the residual falls on the last, per
/// `Money::allocate`). A zero amount allocates zeros without touching the weights, so a bill with no
/// discount does not trip the non-positive-weight guard when every base happens to be zero.
fn allocate_across_classes(
    amount: Money,
    class_bases: &[ClassBase],
) -> Result<Vec<Money>, DomainError> {
    if amount.is_zero() {
        return Ok(vec![Money::zero(amount.currency_code); class_bases.len()]);
    }
    let weights: Vec<i64> = class_bases
        .iter()
        .map(|base| base.amount.amount_minor)
        .collect();
    Ok(amount.allocate(&weights)?)
}

/// The tax on one taxable amount, the base to print beside it, and its named breakdown.
///
/// Shared by the per-class loop and the standalone service-charge line so the two cannot drift on
/// the posture, which is exactly the kind of divergence that produces a receipt whose service charge
/// is taxed on a different basis from the food above it.
fn tax_for(
    taxable: Money,
    rate: pos_proto::locale::TaxRate,
    components: &[TaxComponent],
    prices_include_tax: bool,
    mode: Rounding,
) -> Result<(Money, Money, Vec<TaxComponentLine>), DomainError> {
    // Exclusive: the rate is applied on top of the base. Inclusive: the base already contains the
    // tax, so the tax is extracted from it and the guest's total does not move (ADR-0104).
    let tax = if prices_include_tax {
        taxable.tax_included(rate.as_ratio(), mode)?
    } else {
        taxable.mul_ratio(rate.as_ratio(), mode)?
    };
    // The invoice prints the base the tax was charged *on*, which under an inclusive posture is the
    // quoted amount less the tax inside it — not the quoted amount itself.
    let reported_base = if prices_include_tax {
        taxable.checked_sub(tax)?
    } else {
        taxable
    };
    Ok((tax, reported_base, split_components(tax, components)?))
}

/// The tax lines for classes a charge is taxed at but no line on the bill is: the service charge's,
/// and any class a fee is taxed at that the bill has no line in.
///
/// Without these the charge would be silently untaxed — it is added after discounts and before
/// tax, so a class nobody ordered from carries it. One line per class, so its tax rounds once; the
/// service charge's class comes first, so a bill without fees has exactly the line it always had. A
/// charge that is untaxed, zero, or in a class the bill has needs none here, because the per-class
/// loop folds it in.
///
/// # Errors
///
/// [`DomainError::TaxRateNotConfigured`] if such a class has no rate on this channel, and
/// [`DomainError::Money`] on overflow — the same refusals the per-class loop makes, for the same
/// reason: a charge taxed at no rate is a tax-audit finding, not a default.
fn extra_class_lines(
    input: &BillInput<'_>,
    fee_shares: &[ClassBase],
) -> Result<Vec<TaxLine>, DomainError> {
    let on_bill = |class: TaxClassId| {
        input
            .class_bases
            .iter()
            .any(|base| base.tax_class_id == class)
    };
    // Every charge taxed at a class no line is in, summed per class: the service charge first.
    let mut charges: Vec<ClassBase> = Vec::new();
    if input.service_charge_taxable
        && !input.service_charge.is_zero()
        && let Some(class) = input.service_charge_tax_class
        && !on_bill(class)
    {
        add_to_class(&mut charges, class, input.service_charge)?;
    }
    for share in fee_shares {
        if !share.amount.is_zero() && !on_bill(share.tax_class_id) {
            add_to_class(&mut charges, share.tax_class_id, share.amount)?;
        }
    }

    let mut lines = Vec::with_capacity(charges.len());
    for charge in charges {
        let rate = input
            .rates
            .rate_for(charge.tax_class_id, input.sales_channel)
            .ok_or_else(|| DomainError::TaxRateNotConfigured {
                tax_class_id: charge.tax_class_id.to_string(),
                sales_channel: pos_proto::wire_enum::WireEnum::as_wire(input.sales_channel)
                    .to_owned(),
            })?;
        let (tax, taxable_base, components) = tax_for(
            charge.amount,
            rate,
            input
                .rates
                .components_for(charge.tax_class_id, input.sales_channel),
            input.prices_include_tax,
            input.rounding_mode,
        )?;
        lines.push(TaxLine {
            tax_class_id: charge.tax_class_id,
            taxable_base,
            rate_basis_points: rate.basis_points(),
            tax,
            components,
        });
    }
    Ok(lines)
}

/// What the fee rules add to a bill before tax.
struct Fees {
    /// One line per rule that applied, in rule order.
    lines: Vec<FeeLine>,
    /// Every fee's taxed parts, summed per class.
    class_shares: Vec<ClassBase>,
    /// What the lines charged together.
    total: Money,
}

/// Charges the bill's fee rules (ADR-0159 decision 2). A percentage is taken of lines, never of
/// another fee, so the fees do not compound and their order decides only the order of their lines.
///
/// A waived rule is left out before anything else is read, so a bill whose every fee is waived is
/// assembled exactly as one that froze none.
fn charge_fees(input: &BillInput<'_>, subtotal: Money) -> Result<Fees, DomainError> {
    let mut fees = Fees {
        lines: Vec::new(),
        class_shares: Vec::new(),
        total: Money::zero(input.currency_code),
    };
    let rules: Vec<&FrozenFee> = input
        .fee_rules
        .iter()
        .filter(|rule| !input.waived_fee_ids.contains(&rule.fee_id))
        .collect();
    if rules.is_empty() {
        return Ok(fees);
    }
    if !lines_match_bases(input.lines, input.class_bases)? {
        return Err(DomainError::LinesDoNotMatchBases);
    }
    let reductions = input.bill_discount.checked_add(input.comps)?;
    for rule in rules {
        if let Some(line) = fee_line(rule, input, subtotal, reductions)? {
            fees.total = fees.total.checked_add(line.amount)?;
            for share in &line.class_shares {
                add_to_class(&mut fees.class_shares, share.tax_class_id, share.amount)?;
            }
            fees.lines.push(line);
        }
    }
    Ok(fees)
}

/// What one rule charges on the bill, or `None` when it applies to nothing.
fn fee_line(
    rule: &FrozenFee,
    input: &BillInput<'_>,
    subtotal: Money,
    reductions: Money,
) -> Result<Option<FeeLine>, DomainError> {
    let counted: Vec<&BillLine> = input
        .lines
        .iter()
        .filter(|line| rule.counts_item(line.menu_item_id))
        .collect();
    // Refused by the shape check, or counting no line.
    if !rule.violations().is_empty() || counted.is_empty() {
        return Ok(None);
    }
    let Some(kind) = rule.kind() else {
        return Ok(None);
    };
    let amount = match kind {
        FeeKind::Percent => {
            // The base is taken in the posture the lines are priced in: this release does not
            // convert a tax-inclusive price to a net one, or back.
            let Some(rate) = rule.rate else {
                return Ok(None);
            };
            if rule.base_tax_inclusive != input.prices_include_tax {
                return Ok(None);
            }
            let reductions = if rule.base_discounted {
                reductions
            } else {
                Money::zero(input.currency_code)
            };
            let counted_net = sum_nets(&counted, input.currency_code)?;
            percent_of(counted_net, subtotal, reductions, rate, input.rounding_mode)?
        }
        FeeKind::AmountPerBill | FeeKind::AmountPerUnit => {
            let Some(amount) = rule.amount else {
                return Ok(None);
            };
            if kind == FeeKind::AmountPerUnit {
                let units = counted
                    .iter()
                    .try_fold(Quantity::ZERO, |sum, line| sum.checked_add(line.quantity))?;
                amount.mul_quantity(units, input.rounding_mode)?
            } else {
                amount
            }
        }
        FeeKind::Unspecified => return Ok(None),
    };
    if amount.currency_code != input.currency_code {
        return Ok(None);
    }
    let class_shares = match (rule.tax(), rule.tax_class_id) {
        (FeeTax::NotTaxable, _) => Vec::new(),
        (FeeTax::TaxClass, Some(tax_class_id)) => vec![FeeClassShare {
            tax_class_id,
            amount,
            tax: Money::zero(amount.currency_code),
        }],
        (FeeTax::TaxClass, None) => return Ok(None),
        (FeeTax::FollowLines | FeeTax::Unspecified, _) => {
            follow_lines(amount, &counted, kind == FeeKind::AmountPerUnit)?
        }
    };
    Ok(Some(FeeLine {
        fee_id: rule.fee_id,
        code: rule.code.clone(),
        display_name: rule.display_name.clone(),
        amount,
        class_shares,
        tax: Money::zero(amount.currency_code),
        waivable: rule.waivable,
    }))
}

/// `rate` of the counted lines' net after their share of the bill's reductions, rounded once.
///
/// The reductions fall on the lines in proportion to their nets, as they fall on the classes, so
/// the base is `counted × (subtotal − reductions) / subtotal`. The share and the rate are taken in
/// one division, so the fee is rounded once rather than once for its base and again for its rate.
fn percent_of(
    counted: Money,
    subtotal: Money,
    reductions: Money,
    rate: Ratio,
    mode: Rounding,
) -> Result<Money, DomainError> {
    if reductions.is_zero() {
        return Ok(counted.mul_ratio(rate, mode)?);
    }
    let after = subtotal.checked_sub(reductions)?;
    let numerator = i128::from(counted.amount_minor)
        .checked_mul(i128::from(after.amount_minor))
        .and_then(|value| value.checked_mul(i128::from(rate.numerator())))
        .ok_or(MoneyError::Overflow)?;
    let denominator = i128::from(subtotal.amount_minor)
        .checked_mul(i128::from(rate.denominator().get()))
        .ok_or(MoneyError::Overflow)?;
    Ok(Money::new(
        counted.currency_code,
        div_round(numerator, denominator, mode)?,
    ))
}

/// Spreads a fee across the classes of the lines it counts, in proportion to each class's part of
/// what the fee was charged on: the lines' nets, or their quantities for a fee per unit.
///
/// The bill's reductions fall on every class in proportion to its base, so they leave these
/// proportions as they are. With nothing to weigh by, when every counted line is free, the fee is
/// spread evenly. The shares sum exactly to `amount`, as `Money::allocate` guarantees.
fn follow_lines(
    amount: Money,
    counted: &[&BillLine],
    by_quantity: bool,
) -> Result<Vec<FeeClassShare>, DomainError> {
    let mut weights: Vec<(TaxClassId, i64)> = Vec::new();
    for line in counted {
        let part = if by_quantity {
            line.quantity.as_milli()
        } else {
            line.net.amount_minor
        };
        match weights
            .iter_mut()
            .find(|(class, _)| *class == line.tax_class_id)
        {
            Some((_, weight)) => *weight = weight.checked_add(part).ok_or(MoneyError::Overflow)?,
            None => weights.push((line.tax_class_id, part)),
        }
    }
    let mut parts: Vec<i64> = weights.iter().map(|(_, weight)| *weight).collect();
    let total: i128 = parts.iter().map(|part| i128::from(*part)).sum();
    if total <= 0 || parts.iter().any(|part| *part < 0) {
        parts = vec![1; parts.len()];
    }
    let shares = amount.allocate(&parts)?;
    Ok(weights
        .into_iter()
        .zip(shares)
        .map(|((tax_class_id, _), amount)| FeeClassShare {
            tax_class_id,
            amount,
            tax: Money::zero(amount.currency_code),
        })
        .collect())
}

/// Adds `amount` to its class's entry in per-class totals, or starts one.
fn add_to_class(
    sums: &mut Vec<ClassBase>,
    tax_class_id: TaxClassId,
    amount: Money,
) -> Result<(), DomainError> {
    match sums.iter_mut().find(|sum| sum.tax_class_id == tax_class_id) {
        Some(sum) => sum.amount = sum.amount.checked_add(amount)?,
        None => sums.push(ClassBase {
            tax_class_id,
            amount,
        }),
    }
    Ok(())
}

/// Whether the lines describe the bill the class bases do: per class, the same total.
fn lines_match_bases(lines: &[BillLine], class_bases: &[ClassBase]) -> Result<bool, DomainError> {
    let mut sums: Vec<ClassBase> = Vec::new();
    for line in lines {
        add_to_class(&mut sums, line.tax_class_id, line.net)?;
    }
    Ok(sums.len() == class_bases.len()
        && class_bases.iter().all(|base| {
            sums.iter()
                .any(|sum| sum.tax_class_id == base.tax_class_id && sum.amount == base.amount)
        }))
}

/// Shares each class's tax among the parts of its base, in proportion to them (ADR-0159
/// decision 2), and sums each fee's parts into its tax.
///
/// The parts of a class's base are every fee's part in rule order, then the service charge when
/// it is taxed there, then the lines after their reductions, last. The class's tax rounded once on
/// the whole base and is not touched: each part's share is floored and the last part with a base
/// takes what is left, so the parts sum exactly to the class's tax ([`split_tax`]). The lines come
/// last so that what is left lands on the one part no record itemises: wherever a class's lines
/// come to more than nothing, each fee's part takes exactly its floored share.
fn tax_the_fees(
    fee_lines: &mut [FeeLine],
    tax_lines: &[TaxLine],
    input: &BillInput<'_>,
    lines_after_reductions: &[Money],
) -> Result<(), DomainError> {
    for tax_line in tax_lines {
        let class = tax_line.tax_class_id;
        let mut slots = Vec::new();
        let mut parts = Vec::new();
        for (fee_index, fee) in fee_lines.iter().enumerate() {
            for (share_index, share) in fee.class_shares.iter().enumerate() {
                if share.tax_class_id == class {
                    slots.push((fee_index, share_index));
                    parts.push(share.amount);
                }
            }
        }
        if slots.is_empty() {
            continue;
        }
        if input.service_charge_taxable
            && !input.service_charge.is_zero()
            && input.service_charge_tax_class == Some(class)
        {
            parts.push(input.service_charge);
        }
        if let Some(lines) = input
            .class_bases
            .iter()
            .position(|base| base.tax_class_id == class)
            .and_then(|index| lines_after_reductions.get(index))
        {
            parts.push(*lines);
        }
        for ((fee_index, share_index), tax) in
            slots.into_iter().zip(split_tax(tax_line.tax, &parts)?)
        {
            if let Some(share) = fee_lines
                .get_mut(fee_index)
                .and_then(|fee| fee.class_shares.get_mut(share_index))
            {
                share.tax = tax;
            }
        }
    }
    for fee in fee_lines {
        fee.tax = fee
            .class_shares
            .iter()
            .try_fold(Money::zero(fee.amount.currency_code), |sum, share| {
                sum.checked_add(share.tax)
            })?;
    }
    Ok(())
}

/// Splits a class's tax among the parts of its base, in proportion: each share floored, and the
/// last part that weighs anything taking what is left, so a part of nothing carries no tax. A part
/// below zero, which only a reduction larger than the lines makes, weighs nothing; with nothing to
/// weigh by, the last part takes it all.
fn split_tax(tax: Money, parts: &[Money]) -> Result<Vec<Money>, DomainError> {
    let mut shares = vec![Money::zero(tax.currency_code); parts.len()];
    let mut weights: Vec<i64> = parts.iter().map(|part| part.amount_minor.max(0)).collect();
    while weights.last() == Some(&0) {
        weights.pop();
    }
    if weights.is_empty() {
        if let Some(last) = shares.last_mut() {
            *last = tax;
        }
    } else {
        for (share, part) in shares.iter_mut().zip(tax.allocate(&weights)?) {
            *share = part;
        }
    }
    Ok(shares)
}

/// The lines' nets added up.
fn sum_nets(lines: &[&BillLine], currency: CurrencyCode) -> Result<Money, DomainError> {
    Ok(lines
        .iter()
        .try_fold(Money::zero(currency), |sum, line| sum.checked_add(line.net))?)
}

/// Assembles a bill's totals, charging its fees, computing tax per class and materialising cash
/// rounding.
///
/// # Errors
///
/// - [`DomainError::Empty`] if `class_bases` is empty.
/// - [`DomainError::Money`] on any currency mismatch or overflow.
/// - [`DomainError::TaxRateNotConfigured`] if a class has no rate on this channel — the domain
///   refuses rather than silently charging no tax.
/// - [`DomainError::LinesDoNotMatchBases`] if there are fee rules and the lines do not sum to the
///   class bases.
pub fn assemble(input: &BillInput<'_>) -> Result<BillTotals, DomainError> {
    let currency = input.currency_code;
    if input.class_bases.is_empty() {
        return Err(DomainError::Empty {
            what: "class_bases",
        });
    }

    // Subtotal, checking every base is in the bill's currency.
    let mut subtotal = Money::zero(currency);
    for base in input.class_bases {
        subtotal = subtotal.checked_add(base.amount)?;
    }

    // Bill-level discount and comps, allocated proportionally across classes.
    let discount_shares = allocate_across_classes(input.bill_discount, input.class_bases)?;
    let comp_shares = allocate_across_classes(input.comps, input.class_bases)?;

    // The fees, from the bill before tax: a percentage is taken of lines, never of another fee.
    let fees = charge_fees(input, subtotal)?;

    // Per-class taxable base = base − discount share − comp share, plus the service charge if it
    // is taxable here, plus every fee's part taxed at this class.
    let mut tax_lines = Vec::with_capacity(input.class_bases.len());
    let mut lines_after_reductions = Vec::with_capacity(input.class_bases.len());
    let mut tax_total = Money::zero(currency);
    for (index, base) in input.class_bases.iter().enumerate() {
        let discount = discount_shares
            .get(index)
            .copied()
            .unwrap_or(Money::zero(currency));
        let comp = comp_shares
            .get(index)
            .copied()
            .unwrap_or(Money::zero(currency));
        let mut taxable = base.amount.checked_sub(discount)?.checked_sub(comp)?;
        lines_after_reductions.push(taxable);

        if input.service_charge_taxable && input.service_charge_tax_class == Some(base.tax_class_id)
        {
            taxable = taxable.checked_add(input.service_charge)?;
        }
        if let Some(share) = fees
            .class_shares
            .iter()
            .find(|share| share.tax_class_id == base.tax_class_id)
        {
            taxable = taxable.checked_add(share.amount)?;
        }

        let rate = input
            .rates
            .rate_for(base.tax_class_id, input.sales_channel)
            .ok_or_else(|| DomainError::TaxRateNotConfigured {
                tax_class_id: base.tax_class_id.to_string(),
                sales_channel: pos_proto::wire_enum::WireEnum::as_wire(input.sales_channel)
                    .to_owned(),
            })?;
        let (tax, taxable_base, components) = tax_for(
            taxable,
            rate,
            input
                .rates
                .components_for(base.tax_class_id, input.sales_channel),
            input.prices_include_tax,
            input.rounding_mode,
        )?;
        tax_total = tax_total.checked_add(tax)?;
        tax_lines.push(TaxLine {
            tax_class_id: base.tax_class_id,
            taxable_base,
            rate_basis_points: rate.basis_points(),
            tax,
            components,
        });
    }

    // A charge taxed at a class the bill has no line in needs a line of its own; see
    // `extra_class_lines`.
    for line in extra_class_lines(input, &fees.class_shares)? {
        tax_total = tax_total.checked_add(line.tax)?;
        tax_lines.push(line);
    }

    // Under an inclusive posture the quoted amounts already contain every minor unit of `tax_total`,
    // so the reported subtotal is netted by it. That keeps *one* reconciliation formula true in both
    // postures — `subtotal − discount − comps + service_charge + tax + rounding = total_due` — and
    // leaves the guest paying exactly what the menu said. It also means `subtotal` reads as "what was
    // quoted, net of all tax", which is what 小計 means on a Japanese receipt that folds in a service
    // charge (ADR-0104).
    if input.prices_include_tax {
        subtotal = subtotal.checked_sub(tax_total)?;
    }

    // Each fee's part of each class's base carries its share of that class's tax.
    let mut fee_lines = fees.lines;
    tax_the_fees(&mut fee_lines, &tax_lines, input, &lines_after_reductions)?;

    // Grand total before cash rounding. The service charge carries every fee (ADR-0159 decision 4).
    let service_charge = input.service_charge.checked_add(fees.total)?;
    let unrounded = subtotal
        .checked_sub(input.bill_discount)?
        .checked_sub(input.comps)?
        .checked_add(service_charge)?
        .checked_add(tax_total)?;

    // Cash rounding, materialised as an explicit adjustment so the receipt reconciles.
    let (total_due, rounding_adjustment) = match input.cash_rounding_increment {
        Some(increment) => match core::num::NonZeroI64::new(increment) {
            Some(step) => {
                let rounded = unrounded.round_to_increment(step, input.rounding_mode)?;
                (rounded, rounded.checked_sub(unrounded)?)
            }
            None => (unrounded, Money::zero(currency)),
        },
        None => (unrounded, Money::zero(currency)),
    };

    Ok(BillTotals {
        subtotal,
        discount_total: input.bill_discount,
        comp_total: input.comps,
        service_charge,
        fee_lines,
        tax_lines,
        tax_total,
        rounding_adjustment,
        total_due,
    })
}

/// One payment against a bill.
///
/// `tendered` is what the guest handed over; `applied_to_bill` is what was put against the total,
/// and `tip` is what they left. Change and over-tender live in the gap between them — the
/// distinction ADR-0028 requires, because one field cannot be all three.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Payment {
    /// How it was paid.
    pub method: PaymentMethod,
    /// What the guest handed over.
    pub tendered: Money,
    /// What was applied to the bill.
    pub applied_to_bill: Money,
    /// The tip taken **on this tender**, held apart from the sale and never part of `total_due`
    /// ([ADR-0028](../../../docs/adr/0028-settlement-and-payment-invariant.md)).
    ///
    /// On the payment, not beside it (roadmap **B1.3**). Tips used to travel as a parallel
    /// `Vec<Money>` alongside the payments, with no correspondence between the two: the settlement
    /// arithmetic came out right in total, but nothing knew *which* tender a tip was taken on. Two
    /// consequences followed. `billing.payment.captured.tip_amount` had no value to record and was
    /// written as zero on every payment ever captured, so tip-out could not be reconstructed from
    /// the log. And each payment's `change_given` was computed as `tendered − applied_to_bill`,
    /// which over-reports the change by exactly the tip whenever a tip was taken on that tender —
    /// the drawer would be told to hand back money the guest had just left behind.
    pub tip: Money,
}

impl Payment {
    /// The change owed back on this tender: `tendered − applied_to_bill − tip`.
    ///
    /// Here rather than at each caller so the rule is stated once. A guest who hands over 200,000
    /// on a 165,000 bill and leaves 20,000 gets **15,000** back, not 35,000 — computing it as
    /// `tendered − applied_to_bill` is the second half of the B1.3 defect, and it was computed that
    /// way at the one place that recorded it.
    ///
    /// The result can be negative when a tender does not cover its own share plus its tip; that is
    /// a caller's call to interpret, not this function's to hide. [`settle`] refuses a settlement
    /// whose change is negative *in total*.
    ///
    /// # Errors
    ///
    /// [`DomainError::Money`] on a currency mismatch or overflow.
    pub fn change(&self) -> Result<Money, DomainError> {
        Ok(self
            .tendered
            .checked_sub(self.applied_to_bill)?
            .checked_sub(self.tip)?)
    }
}

/// The result of settling a bill: what the payments proved, and the change owed back.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Settlement {
    /// The total applied to the bill. Equals the bill's `total_due` — a settlement that did not
    /// would not be returned.
    pub total_applied: Money,
    /// The total tendered across all payments.
    pub total_tendered: Money,
    /// The tips taken, a separate ledger from the sale.
    pub total_tips: Money,
    /// Change to hand back: `tendered − applied − tips`.
    pub change_given: Money,
}

/// Proves the settlement invariant and computes the change.
///
/// The invariant ([ADR-0028](../../../docs/adr/0028-settlement-and-payment-invariant.md)): the
/// applied payments sum **exactly** to `total_due`, and `change = tendered − applied − tips`. Tips
/// are a separate ledger, never part of `total_due`.
///
/// # Errors
///
/// - [`DomainError::Empty`] if there are no payments.
/// - [`DomainError::Money`] on a currency mismatch or overflow.
/// - [`DomainError::PaymentsDoNotSumToTotal`] if the applied amounts do not equal `total_due`.
/// - [`DomainError::NegativeChange`] if tendered is less than applied plus tips, which is not a real
///   payment.
pub fn settle(total_due: Money, payments: &[Payment]) -> Result<Settlement, DomainError> {
    if payments.is_empty() {
        return Err(DomainError::Empty { what: "payments" });
    }
    let currency = total_due.currency_code;

    let mut total_applied = Money::zero(currency);
    let mut total_tendered = Money::zero(currency);
    let mut total_tips = Money::zero(currency);
    for payment in payments {
        total_applied = total_applied.checked_add(payment.applied_to_bill)?;
        total_tendered = total_tendered.checked_add(payment.tendered)?;
        total_tips = total_tips.checked_add(payment.tip)?;
    }

    if total_applied != total_due {
        return Err(DomainError::PaymentsDoNotSumToTotal {
            applied_minor: total_applied.amount_minor,
            total_due_minor: total_due.amount_minor,
        });
    }

    // change = tendered − applied − tips, and it cannot be negative.
    let change_given = total_tendered
        .checked_sub(total_applied)?
        .checked_sub(total_tips)?;
    if change_given.is_negative() {
        return Err(DomainError::NegativeChange);
    }

    Ok(Settlement {
        total_applied,
        total_tendered,
        total_tips,
        change_given,
    })
}

/// The fee rules each part of a split bill keeps
/// ([ADR-0159](../../../docs/adr/0159-a-fee-is-configuration.md) decision 6): the rules the bill
/// froze when it opened, one list per part of `parts`, which are the lines each part covers.
///
/// A percentage and a fee per unit are kept by every part as they are, and each part charges them
/// on its own lines. A fee per bill is not charged in full on every part: its amount is allocated
/// across the parts in proportion to the nets of the lines it counts on each, so together they
/// charge exactly what the whole bill would have (`Money::allocate`), and each part keeps the rule
/// for its share alone. A part that counts none of its lines, or whose share is nothing, does not
/// keep it. When every line it counts is free, the parts that count one share it evenly.
///
/// A rule that [`FrozenFee::violations`] faults is kept by every part as it is: it applies to
/// nothing on the whole bill, and to nothing on a part.
///
/// A fee waived on the bill stays waived on every part (ADR-0159 decision 5): a waive follows the
/// fee's `fee_id`, not the rule's amount, so each part keeps the bill's
/// [`BillInput::waived_fee_ids`] as they are, and whatever share of a waived fee per bill it keeps
/// charges nothing.
///
/// # Errors
///
/// [`DomainError::Money`] if the nets a rule counts overflow.
pub fn split_fee_rules(
    rules: &[FrozenFee],
    parts: &[Vec<BillLine>],
) -> Result<Vec<Vec<FrozenFee>>, DomainError> {
    let mut kept: Vec<Vec<FrozenFee>> = vec![Vec::with_capacity(rules.len()); parts.len()];
    for rule in rules {
        let Some(amount) = per_bill_amount(rule) else {
            for part in &mut kept {
                part.push(rule.clone());
            }
            continue;
        };
        let mut counts = Vec::with_capacity(parts.len());
        let mut weights = Vec::with_capacity(parts.len());
        for lines in parts {
            let counted: Vec<&BillLine> = lines
                .iter()
                .filter(|line| rule.counts_item(line.menu_item_id))
                .collect();
            counts.push(!counted.is_empty());
            let weight = counted.iter().try_fold(0_i64, |sum, line| {
                sum.checked_add(line.net.amount_minor.max(0))
                    .ok_or(MoneyError::Overflow)
            })?;
            weights.push(weight);
        }
        if weights.iter().all(|weight| *weight == 0) {
            weights = counts.iter().map(|counts| i64::from(*counts)).collect();
        }
        // The parts after the last that weighs anything take nothing, so what the allocation
        // leaves over lands on a part that counts the rule's lines.
        while weights.last() == Some(&0) {
            weights.pop();
        }
        if weights.is_empty() {
            continue;
        }
        for (part, share) in kept.iter_mut().zip(amount.allocate(&weights)?) {
            if !share.is_zero() {
                part.push(FrozenFee {
                    amount: Some(share),
                    ..rule.clone()
                });
            }
        }
    }
    Ok(kept)
}

/// A rule's amount when it is a fee per bill this release can apply, and `None` otherwise.
fn per_bill_amount(rule: &FrozenFee) -> Option<Money> {
    (rule.kind() == Some(FeeKind::AmountPerBill) && rule.violations().is_empty())
        .then_some(rule.amount)
        .flatten()
}

/// The amount a fee per bill's shares are parts of
/// ([ADR-0159](../../../docs/adr/0159-a-fee-is-configuration.md) decision 6): what it came to on
/// the bill they were first split from. A bill split from no other holds each fee per bill whole.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FeeWhole {
    /// The fee per bill.
    pub fee_id: FeeId,
    /// What it came to on the bill its shares were first split from.
    pub amount: Money,
}

/// The wholes of a bill split from no other: each fee per bill it holds, at its own amount. A part
/// of a split keeps its source's instead, which is what lets a merge put the parts back together.
#[must_use]
pub fn fee_wholes(rules: &[FrozenFee]) -> Vec<FeeWhole> {
    rules
        .iter()
        .filter_map(|rule| {
            per_bill_amount(rule).map(|amount| FeeWhole {
                fee_id: rule.fee_id,
                amount,
            })
        })
        .collect()
}

/// One bill a merge folds together: the fee rules it holds, and the wholes its fees per bill are
/// shares of.
#[derive(Debug, Clone, Copy)]
pub struct MergingBill<'a> {
    /// The rules the bill holds, a fee per bill at its share.
    pub fee_rules: &'a [FrozenFee],
    /// The wholes of its fees per bill ([`fee_wholes`] for a bill split from no other).
    pub fee_wholes: &'a [FeeWhole],
    /// The fees waived on it (ADR-0159 decision 5).
    pub waived_fee_ids: &'a [FeeId],
}

/// What a merged bill holds: its fee rules, and the wholes its fees per bill are shares of.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MergedFees {
    /// The rules the merged bill is computed from.
    pub fee_rules: Vec<FrozenFee>,
    /// The wholes it keeps, so a later split or merge puts the shares together again.
    pub fee_wholes: Vec<FeeWhole>,
    /// The fees waived on the merged bill: every fee waived on any bill merged, the holder's
    /// first, each once (ADR-0159 decision 5). A waive follows the fee, so a fee waived on one
    /// part of a split stays waived once the parts are put back together.
    pub waived_fee_ids: Vec<FeeId>,
}

/// The fee rules a bill holds once `absorbed` are merged into it, the `holder` being the bill the
/// cashier holds ([ADR-0159](../../../docs/adr/0159-a-fee-is-configuration.md) decision 6).
///
/// The merged bill keeps the holder's rules, so a percentage and a fee per unit are charged as the
/// holder's say, on every line it now owes. A fee per bill is charged once on the merged bill, at
/// the sum of the merged bills' shares of it, capped at its whole:
///
/// - merging every part of a split back together restores the whole the bill opened with;
/// - merging some of the parts charges the sum of their shares;
/// - merging bills split from no common bill charges it once, never once per bill;
/// - a fee per bill the holder dropped, its share being nothing, is still charged on the lines of
///   the bills that carry it, which join the holder's rules after its own.
///
/// Where bills split from different bills hold different wholes, after a publish between their
/// openings, the cap is the largest of them. The merged bill keeps every whole any of them held,
/// at the largest, for the next split or merge.
///
/// A fee waived on any of the bills stays waived on the merged bill
/// ([`MergedFees::waived_fee_ids`]): a waive is not undone by putting bills together, so the
/// merged bill charges it nothing, on every line it now owes.
///
/// # Errors
///
/// [`DomainError::Money`] if the shares of a fee overflow or are in different currencies.
pub fn merge_fee_rules(
    holder: MergingBill<'_>,
    absorbed: &[MergingBill<'_>],
) -> Result<MergedFees, DomainError> {
    let bills: Vec<MergingBill<'_>> = core::iter::once(holder)
        .chain(absorbed.iter().copied())
        .collect();
    let mut fee_rules = holder.fee_rules.to_vec();
    for bill in absorbed {
        for rule in bill.fee_rules {
            if per_bill_amount(rule).is_some()
                && !fee_rules.iter().any(|kept| kept.fee_id == rule.fee_id)
            {
                fee_rules.push(rule.clone());
            }
        }
    }
    let mut fee_wholes: Vec<FeeWhole> = Vec::new();
    for whole in bills.iter().flat_map(|bill| bill.fee_wholes) {
        match fee_wholes
            .iter_mut()
            .find(|kept| kept.fee_id == whole.fee_id)
        {
            Some(kept) => {
                if kept.amount.currency_code == whole.amount.currency_code
                    && whole.amount.amount_minor > kept.amount.amount_minor
                {
                    kept.amount = whole.amount;
                }
            }
            None => fee_wholes.push(*whole),
        }
    }
    for rule in &mut fee_rules {
        let Some(own) = per_bill_amount(rule) else {
            continue;
        };
        let mut shares = Money::zero(own.currency_code);
        for bill in &bills {
            if let Some(share) = bill
                .fee_rules
                .iter()
                .find(|held| held.fee_id == rule.fee_id)
                .and_then(per_bill_amount)
            {
                shares = shares.checked_add(share)?;
            }
        }
        let capped = match fee_wholes.iter().find(|whole| whole.fee_id == rule.fee_id) {
            Some(whole)
                if whole.amount.currency_code == shares.currency_code
                    && whole.amount.amount_minor < shares.amount_minor =>
            {
                whole.amount
            }
            _ => shares,
        };
        rule.amount = Some(capped);
    }
    let mut waived_fee_ids: Vec<FeeId> = Vec::new();
    for fee_id in bills.iter().flat_map(|bill| bill.waived_fee_ids) {
        if !waived_fee_ids.contains(fee_id) {
            waived_fee_ids.push(*fee_id);
        }
    }
    Ok(MergedFees {
        fee_rules,
        fee_wholes,
        waived_fee_ids,
    })
}

/// The rule for `fee_id` as one bill may waive it
/// ([ADR-0159](../../../docs/adr/0159-a-fee-is-configuration.md) decision 5): one of the rules
/// the bill froze, `rules`, not already among its `waived` fees, and published as waivable.
///
/// Asked before anybody is asked to approve the waive, so a fee that could never be waived costs
/// nobody a PIN.
///
/// # Errors
///
/// [`DomainError::FeeNotOnBill`] if the bill froze no rule under `fee_id`, or has waived it
/// already; [`DomainError::FeeNotWaivable`] if the rule does not let staff waive it.
pub fn waivable_fee<'a>(
    rules: &'a [FrozenFee],
    waived: &[FeeId],
    fee_id: FeeId,
) -> Result<&'a FrozenFee, DomainError> {
    let rule = rules
        .iter()
        .find(|rule| rule.fee_id == fee_id)
        .filter(|_| !waived.contains(&fee_id))
        .ok_or(DomainError::FeeNotOnBill)?;
    if rule.waivable {
        Ok(rule)
    } else {
        Err(DomainError::FeeNotWaivable)
    }
}

/// Splits a total into `parts` amounts summing **exactly** to it (`pos-spec.md` §14.3).
///
/// A thin domain wrapper over `Money::split_into`, here so the split law is a domain operation with
/// its own property test rather than only a `pos-proto` one, and so a caller splits a bill without
/// reaching for the money primitive directly.
///
/// # Errors
///
/// [`DomainError::Money`] if `parts` is zero or the arithmetic overflows.
pub fn split_evenly(total_due: Money, parts: usize) -> Result<Vec<Money>, DomainError> {
    Ok(total_due.split_into(parts)?)
}

/// Splits a total across `weights` (by seat, by share) summing **exactly** to it.
///
/// # Errors
///
/// [`DomainError::Money`] if the weights are empty, any is negative, they sum to zero, or the
/// arithmetic overflows.
pub fn split_by_weights(total_due: Money, weights: &[i64]) -> Result<Vec<Money>, DomainError> {
    Ok(total_due.allocate(weights)?)
}

#[cfg(test)]
mod tests {
    use super::{
        BillInput, BillLine, BillTotals, ClassBase, FeeClassShare, FeeLine, FeeWhole, MergedFees,
        MergingBill, Payment, assemble, fee_wholes, merge_fee_rules, settle, split_by_weights,
        split_evenly, split_fee_rules, waivable_fee,
    };
    use crate::error::DomainError;
    use pos_proto::fees::{FeeCode, FeeItems, FeeKind, FeeTax, FrozenFee};
    use pos_proto::locale::{TaxComponent, TaxRate, TaxRateTable};
    use pos_proto::money::{Money, Ratio, Rounding};
    use pos_proto::quantity::Quantity;
    use pos_proto::text::DisplayName;
    use pos_proto::wire_enum::Open;
    use pos_proto::{
        CurrencyCode, FeeId, MenuItemId, PaymentMethod, SalesChannel, TaxClassId, Ulid,
    };
    use proptest::prelude::*;

    const VND: CurrencyCode = CurrencyCode::VND;

    fn food() -> TaxClassId {
        TaxClassId::new(Ulid::from_u128(1))
    }

    fn alcohol() -> TaxClassId {
        TaxClassId::new(Ulid::from_u128(2))
    }

    fn vnd(amount: i64) -> Money {
        Money::new(VND, amount)
    }

    /// Food 10%, alcohol 15%, both on dine-in.
    fn rates() -> TaxRateTable {
        TaxRateTable::new()
            .with(food(), SalesChannel::DineIn, TaxRate::from_percent(10))
            .with(alcohol(), SalesChannel::DineIn, TaxRate::from_percent(15))
    }

    fn base_input<'a>(class_bases: &'a [ClassBase], rates: &'a TaxRateTable) -> BillInput<'a> {
        BillInput {
            currency_code: VND,
            class_bases,
            bill_discount: vnd(0),
            comps: vnd(0),
            service_charge: vnd(0),
            service_charge_taxable: true,
            service_charge_tax_class: Some(food()),
            rates,
            sales_channel: SalesChannel::DineIn,
            cash_rounding_increment: None,
            rounding_mode: Rounding::HalfUp,
            prices_include_tax: false,
            fee_rules: &[],
            waived_fee_ids: &[],
            lines: &[],
        }
    }

    /// Every component reconciles to the total. This is the arithmetic ADR-0028 fixes, and the
    /// property a VAT invoice depends on.
    fn assert_reconciles(totals: &BillTotals) {
        // tax lines sum to tax_total
        let mut tax = vnd(0);
        for line in &totals.tax_lines {
            tax = tax.checked_add(line.tax).expect("in range");
        }
        assert_eq!(
            tax, totals.tax_total,
            "per-class tax lines must sum to the tax total"
        );

        // subtotal − discount − comps + service_charge + tax + rounding == total_due
        let rebuilt = totals
            .subtotal
            .checked_sub(totals.discount_total)
            .and_then(|value| value.checked_sub(totals.comp_total))
            .and_then(|value| value.checked_add(totals.service_charge))
            .and_then(|value| value.checked_add(totals.tax_total))
            .and_then(|value| value.checked_add(totals.rounding_adjustment))
            .expect("in range");
        assert_eq!(
            rebuilt, totals.total_due,
            "components must reconcile to total_due"
        );
    }

    // ---- ADR-0104: the two country facts that could not be configuration ---------------------

    /// India: 18 % GST on an intra-state sale prints as CGST 9 % + SGST 9 %, and the halves sum
    /// exactly to the tax charged. Printing the sum would not be a valid tax invoice.
    #[test]
    fn an_indian_bill_breaks_gst_into_cgst_and_sgst_that_sum_to_the_tax() {
        let rates = TaxRateTable::new().with_components(
            food(),
            SalesChannel::DineIn,
            TaxRate::from_percent(18),
            vec![
                TaxComponent::new("CGST", TaxRate::from_percent(9)),
                TaxComponent::new("SGST", TaxRate::from_percent(9)),
            ],
        );
        let bases = [ClassBase {
            tax_class_id: food(),
            amount: vnd(1_000),
        }];
        let totals = assemble(&base_input(&bases, &rates)).expect("assembles");

        assert_eq!(totals.tax_total, vnd(180), "18% of 1000");
        let line = totals.tax_lines.first().expect("one class, one tax line");
        assert_eq!(line.components.len(), 2);
        let cgst = line.components.first().expect("CGST");
        let sgst = line.components.get(1).expect("SGST");
        assert_eq!(cgst.name, "CGST");
        assert_eq!(cgst.tax, vnd(90));
        assert_eq!(sgst.name, "SGST");
        assert_eq!(sgst.tax, vnd(90));

        let split: i64 = line
            .components
            .iter()
            .map(|part| part.tax.amount_minor)
            .sum();
        assert_eq!(
            split, line.tax.amount_minor,
            "the parts must sum to the tax charged, or the invoice does not add up"
        );
        assert_reconciles(&totals);
    }

    /// An odd amount still splits exactly: the residual lands on the last component rather than
    /// being lost, which is `Money::allocate`'s guarantee and the reason the split is an allocation
    /// rather than two multiplications.
    #[test]
    fn an_odd_tax_still_splits_without_losing_a_minor_unit() {
        let rates = TaxRateTable::new().with_components(
            food(),
            SalesChannel::DineIn,
            TaxRate::from_percent(18),
            vec![
                TaxComponent::new("CGST", TaxRate::from_percent(9)),
                TaxComponent::new("SGST", TaxRate::from_percent(9)),
            ],
        );
        let bases = [ClassBase {
            tax_class_id: food(),
            amount: vnd(105),
        }];
        let totals = assemble(&base_input(&bases, &rates)).expect("assembles");

        let line = totals.tax_lines.first().expect("one class, one tax line");
        let split: i64 = line
            .components
            .iter()
            .map(|part| part.tax.amount_minor)
            .sum();
        assert_eq!(
            split, line.tax.amount_minor,
            "no minor unit is lost or invented"
        );
        assert_reconciles(&totals);
    }

    /// Japan: a 税込 price is what the guest pays. The tax comes *out* of it, the total does not move,
    /// and the printed base is the price net of the tax inside it.
    #[test]
    fn an_inclusive_price_extracts_its_tax_and_leaves_the_total_alone() {
        let rates =
            TaxRateTable::new().with(food(), SalesChannel::DineIn, TaxRate::from_percent(10));
        let bases = [ClassBase {
            tax_class_id: food(),
            amount: vnd(1_100),
        }];
        let mut input = base_input(&bases, &rates);
        input.prices_include_tax = true;
        let totals = assemble(&input).expect("assembles");

        assert_eq!(
            totals.total_due,
            vnd(1_100),
            "the guest pays the quoted price"
        );
        assert_eq!(totals.tax_total, vnd(100), "1100 x 10/110");
        assert_eq!(
            totals.subtotal,
            vnd(1_000),
            "the subtotal is reported net of tax"
        );
        let line = totals.tax_lines.first().expect("one tax line");
        assert_eq!(line.taxable_base, vnd(1_000));
        assert_reconciles(&totals);
    }

    /// The same bill under the two postures charges different totals from the same numbers, which is
    /// the whole point of the flag — and exclusive stays exactly what it was before ADR-0104.
    #[test]
    fn the_posture_is_what_separates_an_inclusive_bill_from_an_exclusive_one() {
        let rates =
            TaxRateTable::new().with(food(), SalesChannel::DineIn, TaxRate::from_percent(10));
        let bases = [ClassBase {
            tax_class_id: food(),
            amount: vnd(1_000),
        }];

        let exclusive = assemble(&base_input(&bases, &rates)).expect("assembles");
        assert_eq!(exclusive.total_due, vnd(1_100), "tax added on top");
        assert_eq!(exclusive.subtotal, vnd(1_000));
        assert!(
            exclusive
                .tax_lines
                .first()
                .expect("one tax line")
                .components
                .is_empty(),
            "no components published means no breakdown, not a breakdown of nothing"
        );

        let mut input = base_input(&bases, &rates);
        input.prices_include_tax = true;
        let inclusive = assemble(&input).expect("assembles");
        assert_eq!(
            inclusive.total_due,
            vnd(1_000),
            "tax taken out of the price"
        );
        assert_eq!(inclusive.tax_total, vnd(91), "1000 x 10/110, half-up");
        assert_reconciles(&exclusive);
        assert_reconciles(&inclusive);
    }

    #[test]
    fn a_single_class_bill_taxes_once() {
        let bases = [ClassBase {
            tax_class_id: food(),
            amount: vnd(100_000),
        }];
        let rates = rates();
        let totals = assemble(&base_input(&bases, &rates)).expect("assembles");
        assert_eq!(totals.subtotal, vnd(100_000));
        assert_eq!(totals.tax_total, vnd(10_000), "10% of 100,000");
        assert_eq!(totals.total_due, vnd(110_000));
        assert_reconciles(&totals);
    }

    #[test]
    fn tax_is_computed_per_class() {
        let bases = [
            ClassBase {
                tax_class_id: food(),
                amount: vnd(100_000),
            },
            ClassBase {
                tax_class_id: alcohol(),
                amount: vnd(200_000),
            },
        ];
        let rates = rates();
        let totals = assemble(&base_input(&bases, &rates)).expect("assembles");
        // 10% of 100k = 10k, 15% of 200k = 30k
        assert_eq!(totals.tax_total, vnd(40_000));
        assert_eq!(totals.tax_lines.len(), 2);
        assert_reconciles(&totals);
    }

    #[test]
    fn a_taxable_service_charge_is_taxed_and_an_untaxed_one_is_not() {
        let bases = [ClassBase {
            tax_class_id: food(),
            amount: vnd(100_000),
        }];
        let rates = rates();

        let mut taxable = base_input(&bases, &rates);
        taxable.service_charge = vnd(10_000);
        taxable.service_charge_taxable = true;
        let with_tax = assemble(&taxable).expect("assembles");
        // taxable base for food = 100k + 10k SC = 110k, tax 11k
        assert_eq!(with_tax.tax_total, vnd(11_000));
        assert_eq!(with_tax.total_due, vnd(100_000 + 10_000 + 11_000));
        assert_reconciles(&with_tax);

        let mut untaxed = base_input(&bases, &rates);
        untaxed.service_charge = vnd(10_000);
        untaxed.service_charge_taxable = false;
        let no_tax = assemble(&untaxed).expect("assembles");
        assert_eq!(
            no_tax.tax_total,
            vnd(10_000),
            "SC excluded from the taxable base"
        );
        assert_reconciles(&no_tax);
    }

    #[test]
    fn cash_rounding_is_an_explicit_adjustment() {
        // 100,000 + 10% tax = 110,000 already on a 500 boundary; use an amount that is not.
        let bases = [ClassBase {
            tax_class_id: food(),
            amount: vnd(99_999),
        }];
        let rates = rates();
        let mut input = base_input(&bases, &rates);
        input.cash_rounding_increment = Some(500);
        let totals = assemble(&input).expect("assembles");
        // unrounded = 99,999 + round_half_up(9,999.9) = 99,999 + 10,000 = 109,999
        // rounded to nearest 500 (half up) = 110,000; adjustment = +1
        assert_eq!(totals.rounding_adjustment, vnd(1));
        assert_eq!(
            totals.total_due.amount_minor % 500,
            0,
            "total lands on the increment"
        );
        assert_reconciles(&totals);
    }

    #[test]
    fn a_missing_rate_is_refused_rather_than_charged_zero() {
        let bases = [ClassBase {
            tax_class_id: alcohol(),
            amount: vnd(100_000),
        }];
        // A rate table that prices food but not alcohol.
        let partial =
            TaxRateTable::new().with(food(), SalesChannel::DineIn, TaxRate::from_percent(10));
        let result = assemble(&base_input(&bases, &partial));
        assert!(matches!(
            result,
            Err(DomainError::TaxRateNotConfigured { .. })
        ));
    }

    #[test]
    fn an_empty_bill_is_refused() {
        let rates = rates();
        let result = assemble(&base_input(&[], &rates));
        assert!(matches!(
            result,
            Err(DomainError::Empty {
                what: "class_bases"
            })
        ));
    }

    #[test]
    fn exact_cash_settlement_gives_no_change() {
        let payment = Payment {
            method: PaymentMethod::Cash,
            tendered: vnd(110_000),
            applied_to_bill: vnd(110_000),
            tip: vnd(0),
        };
        let settlement = settle(vnd(110_000), &[payment]).expect("settles");
        assert_eq!(settlement.change_given, vnd(0));
        assert_eq!(settlement.total_applied, vnd(110_000));
    }

    #[test]
    fn over_tender_gives_change_and_a_tip_is_not_part_of_the_total() {
        // Guest owes 110k, hands over 200k cash, leaves a 20k tip.
        let payment = Payment {
            method: PaymentMethod::Cash,
            tendered: vnd(200_000),
            applied_to_bill: vnd(110_000),
            tip: vnd(20_000),
        };
        let settlement = settle(vnd(110_000), &[payment]).expect("settles");
        // change = 200k − 110k applied − 20k tip = 70k
        assert_eq!(settlement.change_given, vnd(70_000));
        assert_eq!(settlement.total_tips, vnd(20_000));
        assert_eq!(
            settlement.total_applied,
            vnd(110_000),
            "the tip is not in what was applied"
        );
    }

    /// The second half of the B1.3 defect, in the smallest form it can be stated.
    ///
    /// The edge recorded each captured payment's change as `tendered − applied_to_bill`, which
    /// over-reports it by exactly the tip. On this payment that is 35,000 against a true 15,000: a
    /// till told to hand back 20,000 the guest had just left behind, on every tipped cash sale.
    #[test]
    fn a_tipped_tender_owes_change_less_the_tip() {
        let payment = Payment {
            method: PaymentMethod::Cash,
            tendered: vnd(200_000),
            applied_to_bill: vnd(165_000),
            tip: vnd(20_000),
        };
        assert_eq!(
            payment.change().expect("in range"),
            vnd(15_000),
            "200,000 − 165,000 applied − 20,000 tip"
        );
    }

    #[test]
    fn an_untipped_tender_owes_the_whole_over_tender() {
        let payment = Payment {
            method: PaymentMethod::Cash,
            tendered: vnd(200_000),
            applied_to_bill: vnd(165_000),
            tip: vnd(0),
        };
        assert_eq!(payment.change().expect("in range"), vnd(35_000));
    }

    #[test]
    fn several_methods_combine_on_one_bill() {
        let card = Payment {
            method: PaymentMethod::Card,
            tendered: vnd(60_000),
            applied_to_bill: vnd(60_000),
            tip: vnd(0),
        };
        let cash = Payment {
            method: PaymentMethod::Cash,
            tendered: vnd(50_000),
            applied_to_bill: vnd(50_000),
            tip: vnd(0),
        };
        let settlement = settle(vnd(110_000), &[card, cash]).expect("settles");
        assert_eq!(settlement.total_applied, vnd(110_000));
        assert_eq!(settlement.change_given, vnd(0));
    }

    #[test]
    fn under_application_is_refused() {
        let payment = Payment {
            method: PaymentMethod::Card,
            tendered: vnd(100_000),
            applied_to_bill: vnd(100_000),
            tip: vnd(0),
        };
        let result = settle(vnd(110_000), &[payment]);
        assert!(matches!(
            result,
            Err(DomainError::PaymentsDoNotSumToTotal { .. })
        ));
    }

    #[test]
    fn tendering_less_than_applied_plus_tips_is_negative_change() {
        // applied equals total, but tendered is below applied + tip: not a real payment.
        let payment = Payment {
            method: PaymentMethod::Cash,
            tendered: vnd(110_000),
            applied_to_bill: vnd(110_000),
            tip: vnd(5_000),
        };
        let result = settle(vnd(110_000), &[payment]);
        assert!(matches!(result, Err(DomainError::NegativeChange)));
    }

    #[test]
    fn settling_nothing_is_refused() {
        assert!(matches!(
            settle(vnd(110_000), &[]),
            Err(DomainError::Empty { what: "payments" })
        ));
    }

    // ---- ADR-0159: fees -----------------------------------------------------------------------

    fn service() -> TaxClassId {
        TaxClassId::new(Ulid::from_u128(3))
    }

    fn item(n: u128) -> MenuItemId {
        MenuItemId::new(Ulid::from_u128(10 + n))
    }

    fn line(n: u128, units: i64, net: i64, tax_class_id: TaxClassId) -> BillLine {
        BillLine {
            menu_item_id: item(n),
            quantity: Quantity::from_whole(units).expect("a small quantity"),
            net: vnd(net),
            tax_class_id,
        }
    }

    /// Two pizzas (item 0) and a bottle of wine (item 1): 200,000 of food at 10 % and 100,000 of
    /// alcohol at 15 %.
    fn dinner() -> Vec<BillLine> {
        vec![line(0, 2, 200_000, food()), line(1, 1, 100_000, alcohol())]
    }

    /// A rule as a bill freezes one by default — every item, following its lines — at `value`
    /// basis points for a percentage, or `value` đồng for an amount. Whether a rule is active and
    /// on the bill's channel was settled when it was frozen (`pos_proto::fees` tests that).
    fn fee(id: u128, kind: FeeKind, value: i64) -> FrozenFee {
        let percent = kind == FeeKind::Percent;
        FrozenFee {
            fee_id: FeeId::new(Ulid::from_u128(id)),
            code: FeeCode::new("FEE"),
            display_name: DisplayName::new("Fee"),
            kind: kind.into(),
            rate: percent.then(|| Ratio::basis_points(value).expect("a rate")),
            amount: (!percent).then_some(vnd(value)),
            item_scope: Open::default(),
            menu_item_ids: Vec::new(),
            base_discounted: true,
            base_tax_inclusive: false,
            tax: Open::default(),
            tax_class_id: None,
            waivable: false,
        }
    }

    fn altered(mut fee: FrozenFee, change: impl FnOnce(&mut FrozenFee)) -> FrozenFee {
        change(&mut fee);
        fee
    }

    fn untaxed(fee: FrozenFee) -> FrozenFee {
        altered(fee, |fee| fee.tax = FeeTax::NotTaxable.into())
    }

    fn taxed_at(tax_class_id: TaxClassId, fee: FrozenFee) -> FrozenFee {
        altered(fee, |fee| {
            fee.tax = FeeTax::TaxClass.into();
            fee.tax_class_id = Some(tax_class_id);
        })
    }

    fn only(scope: FeeItems, n: u128, fee: FrozenFee) -> FrozenFee {
        altered(fee, |fee| {
            fee.item_scope = scope.into();
            fee.menu_item_ids = vec![item(n)];
        })
    }

    /// A fee's part taxed at a class, and that part's tax.
    fn share(tax_class_id: TaxClassId, amount: i64, tax: i64) -> FeeClassShare {
        FeeClassShare {
            tax_class_id,
            amount: vnd(amount),
            tax: vnd(tax),
        }
    }

    /// The tax on each fee that applied, in rule order.
    fn fee_taxes(totals: &BillTotals) -> Vec<Money> {
        totals.fee_lines.iter().map(|fee| fee.tax).collect()
    }

    /// The class bases a caller sums from its lines.
    fn bases_of(lines: &[BillLine]) -> Vec<ClassBase> {
        let mut bases: Vec<ClassBase> = Vec::new();
        for line in lines {
            match bases
                .iter_mut()
                .find(|b| b.tax_class_id == line.tax_class_id)
            {
                Some(base) => base.amount = base.amount.checked_add(line.net).expect("in range"),
                None => bases.push(ClassBase {
                    tax_class_id: line.tax_class_id,
                    amount: line.net,
                }),
            }
        }
        bases
    }

    /// Assembles `lines` under `rules`, with `adjust` setting anything else on the input.
    fn assemble_with(
        lines: &[BillLine],
        rules: &[FrozenFee],
        rates: &TaxRateTable,
        adjust: impl FnOnce(&mut BillInput<'_>),
    ) -> Result<BillTotals, DomainError> {
        assemble_waiving(lines, rules, &[], rates, adjust)
    }

    /// [`assemble_with`], with the fees `waived` waived on the bill.
    fn assemble_waiving(
        lines: &[BillLine],
        rules: &[FrozenFee],
        waived: &[FeeId],
        rates: &TaxRateTable,
        adjust: impl FnOnce(&mut BillInput<'_>),
    ) -> Result<BillTotals, DomainError> {
        let bases = bases_of(lines);
        let mut input = base_input(&bases, rates);
        input.fee_rules = rules;
        input.waived_fee_ids = waived;
        input.lines = lines;
        adjust(&mut input);
        assemble(&input)
    }

    #[test]
    fn a_percentage_is_taken_of_the_lines_after_the_bills_reductions_and_never_of_a_fee() {
        let rates = rates();
        let discounted = |input: &mut BillInput<'_>| input.bill_discount = vnd(30_000);
        let rules = [
            fee(1, FeeKind::Percent, 1_000),
            untaxed(fee(2, FeeKind::Percent, 500)),
        ];
        let totals = assemble_with(&dinner(), &rules, &rates, discounted).expect("assembles");
        // 10 % of 300,000 less the 30,000 off, spread 2:1 as the lines are; then 5 % of the same
        // lines, not of the lines and the first fee. Each part carries its class's rate.
        let ten = FeeLine {
            fee_id: FeeId::new(Ulid::from_u128(1)),
            code: FeeCode::new("FEE"),
            display_name: DisplayName::new("Fee"),
            amount: vnd(27_000),
            class_shares: vec![share(food(), 18_000, 1_800), share(alcohol(), 9_000, 1_350)],
            tax: vnd(1_800 + 1_350),
            waivable: false,
        };
        assert_eq!(totals.fee_lines.first(), Some(&ten));
        assert_eq!(
            totals.fee_lines.get(1).map(|line| (line.amount, line.tax)),
            Some((vnd(13_500), vnd(0)))
        );
        assert_eq!(totals.service_charge, vnd(27_000 + 13_500));
        // Food 200,000 − 20,000 + 18,000 at 10 %, alcohol 100,000 − 10,000 + 9,000 at 15 %.
        assert_eq!(totals.tax_total, vnd(19_800 + 14_850));
        assert_eq!(totals.total_due, vnd(270_000 + 40_500 + 34_650));
        assert_reconciles(&totals);

        let before = altered(fee(1, FeeKind::Percent, 1_000), |f| {
            f.base_discounted = false;
        });
        let totals = assemble_with(&dinner(), &[before], &rates, discounted).expect("assembles");
        assert_eq!(
            totals.service_charge,
            vnd(30_000),
            "10 % of the 300,000 sold"
        );
        assert_reconciles(&totals);
    }

    #[test]
    fn each_tax_treatment_puts_the_fee_where_its_rule_says() {
        let rates = rates().with(service(), SalesChannel::DineIn, TaxRate::from_percent(8));
        let taxed = |tax: FeeTax, class: Option<TaxClassId>| {
            altered(fee(1, FeeKind::AmountPerBill, 10_000), |f| {
                f.tax = tax.into();
                f.tax_class_id = class;
            })
        };
        let cases = [
            (taxed(FeeTax::NotTaxable, None), vec![], 0, 20_000 + 15_000),
            // 110,000 of alcohol taxes 16,500, and the fee is a tenth of it.
            (
                taxed(FeeTax::TaxClass, Some(alcohol())),
                vec![share(alcohol(), 10_000, 1_500)],
                1_500,
                20_000 + 16_500,
            ),
            // A class no line on the bill is in gets a tax line of its own, all the fee's.
            (
                taxed(FeeTax::TaxClass, Some(service())),
                vec![share(service(), 10_000, 800)],
                800,
                20_000 + 15_000 + 800,
            ),
            // Spread 2:1 as the lines are, each class's tax rounded once on its whole base: food
            // 206,666 taxes 20,667, of which the fee's part is 666.6, floored; alcohol 103,334
            // taxes 15,500, of which the fee's part is 500.1.
            (
                taxed(FeeTax::FollowLines, None),
                vec![share(food(), 6_666, 666), share(alcohol(), 3_334, 500)],
                666 + 500,
                20_667 + 15_500,
            ),
        ];
        for (rule, shares, fee_tax, tax) in cases {
            let totals = assemble_with(&dinner(), &[rule], &rates, |_| {}).expect("assembles");
            let charged = totals.fee_lines.first().expect("the fee applies");
            assert_eq!(charged.class_shares, shares);
            assert_eq!(charged.tax, vnd(fee_tax));
            assert_eq!(totals.tax_total, vnd(tax));
            assert_eq!(totals.total_due, vnd(300_000 + 10_000 + tax));
            assert_reconciles(&totals);
        }
    }

    #[test]
    fn a_fee_per_unit_charges_each_unit_it_counts() {
        let rates = rates();
        let boxes = only(FeeItems::Include, 0, fee(1, FeeKind::AmountPerUnit, 3_000));
        let totals = assemble_with(&dinner(), &[boxes], &rates, |_| {}).expect("assembles");
        // Two pizzas, two boxes, taxed with the pizzas they hold.
        let charged = totals.fee_lines.first().expect("the fee applies");
        assert_eq!(charged.amount, vnd(6_000));
        assert_eq!(charged.class_shares, vec![share(food(), 6_000, 600)]);
        assert_eq!(charged.tax, vnd(600));
        assert_eq!(totals.tax_total, vnd(20_600 + 15_000));
        assert_reconciles(&totals);
    }

    #[test]
    fn a_fee_applies_only_to_the_items_it_counts() {
        let rates = rates();
        let charged = |rule: FrozenFee| {
            let totals = assemble_with(&dinner(), &[rule], &rates, |_| {}).expect("assembles");
            assert_reconciles(&totals);
            totals.service_charge
        };
        assert_eq!(charged(fee(1, FeeKind::AmountPerBill, 15_000)), vnd(15_000));
        let wine = only(
            FeeItems::Exclude,
            0,
            untaxed(fee(2, FeeKind::Percent, 1_000)),
        );
        assert_eq!(charged(wine), vnd(10_000), "10 % of the wine alone");
        let absent = only(FeeItems::Include, 9, fee(3, FeeKind::AmountPerBill, 15_000));
        assert_eq!(charged(absent), vnd(0), "it counts no line on this bill");
    }

    #[test]
    fn a_rule_that_cannot_apply_changes_nothing() {
        let rates = rates();
        for inclusive in [false, true] {
            let per_bill = || fee(1, FeeKind::AmountPerBill, 15_000);
            let cannot = [
                altered(per_bill(), |f| f.kind = Open::parse("FEE_KIND_TOP_UP_TO")),
                altered(per_bill(), |f| {
                    f.item_scope = Open::parse("FEE_ITEMS_CATEGORY");
                }),
                altered(per_bill(), |f| f.amount = None),
                altered(per_bill(), |f| {
                    f.amount = Some(Money::new(CurrencyCode::JPY, 500));
                }),
                // A percentage of a base in the other tax posture waits for a release that
                // converts it.
                altered(fee(2, FeeKind::Percent, 1_000), |f| {
                    f.base_tax_inclusive = !inclusive;
                }),
            ];
            let busy = |input: &mut BillInput<'_>| {
                input.bill_discount = vnd(30_001);
                input.comps = vnd(7);
                input.service_charge = vnd(9_999);
                input.cash_rounding_increment = Some(500);
                input.prices_include_tax = inclusive;
            };
            let without = assemble_with(&dinner(), &[], &rates, busy).expect("assembles");
            assert!(without.fee_lines.is_empty());
            assert_eq!(without.service_charge, vnd(9_999));
            for rule in cannot {
                let id = rule.fee_id;
                let charged = assemble_with(&dinner(), &[rule], &rates, busy).expect("assembles");
                assert_eq!(charged, without, "{id} must apply to nothing");
            }
        }
    }

    #[test]
    fn a_fee_rounds_once_by_the_bills_rounding_mode() {
        let rates = rates();
        let charge = |lines: &[BillLine], rule: FrozenFee, mode: Rounding, discount: i64| {
            let set = |input: &mut BillInput<'_>| {
                input.rounding_mode = mode;
                input.bill_discount = vnd(discount);
            };
            let rules = [untaxed(rule)];
            let totals = assemble_with(lines, &rules, &rates, set).expect("assembles");
            totals.service_charge
        };
        // 10 % of 25 is 2.5, a tie the mode breaks.
        let small = [line(0, 1, 25, food())];
        let ten = || fee(1, FeeKind::Percent, 1_000);
        assert_eq!(charge(&small, ten(), Rounding::HalfUp, 0), vnd(3));
        assert_eq!(charge(&small, ten(), Rounding::HalfEven, 0), vnd(2));
        // Half a unit at 3,001 is 1,500.5.
        let half = [BillLine {
            quantity: Quantity::HALF,
            ..line(0, 1, 40_000, food())
        }];
        let box_fee = || fee(1, FeeKind::AmountPerUnit, 3_001);
        assert_eq!(charge(&half, box_fee(), Rounding::HalfUp, 0), vnd(1_501));
        assert_eq!(charge(&half, box_fee(), Rounding::HalfEven, 0), vnd(1_500));
        // 75 % of the first line's share of a 3-đồng bill with 1 off is exactly 0.5, so half-even
        // gives 0. Rounding the base first (0.67 to 1) and then the rate (0.75) would give 1.
        let tiny = [line(0, 1, 1, food()), line(1, 1, 2, food())];
        let first = only(FeeItems::Include, 0, fee(1, FeeKind::Percent, 7_500));
        assert_eq!(charge(&tiny, first, Rounding::HalfEven, 1), vnd(0));
    }

    #[test]
    fn lines_that_are_not_the_bills_are_refused() {
        let rates = rates();
        let bases = bases_of(&dinner());
        let total = |lines: &[BillLine], rules: &[FrozenFee]| {
            let mut input = base_input(&bases, &rates);
            input.fee_rules = rules;
            input.lines = lines;
            assemble(&input).map(|totals| totals.total_due)
        };
        let rules = [fee(1, FeeKind::AmountPerBill, 1_000)];
        let short = [line(0, 2, 199_999, food()), line(1, 1, 100_000, alcohol())];
        let stray = [line(0, 2, 200_000, food()), line(1, 1, 100_000, service())];
        for lines in [&[][..], &short[..], &stray[..]] {
            assert_eq!(total(lines, &rules), Err(DomainError::LinesDoNotMatchBases));
        }
        // With no rule the lines are not read at all.
        assert_eq!(total(&short, &[]), Ok(vnd(335_000)));
    }

    #[test]
    fn an_inclusive_store_takes_a_fee_as_quoted() {
        let rates =
            TaxRateTable::new().with(food(), SalesChannel::DineIn, TaxRate::from_percent(10));
        let quoted = [line(0, 1, 1_100, food())];
        let inclusive = |input: &mut BillInput<'_>| input.prices_include_tax = true;
        // An amount carries its tax inside, as the menu's prices do.
        let per_bill = [fee(1, FeeKind::AmountPerBill, 1_100)];
        let totals = assemble_with(&quoted, &per_bill, &rates, inclusive).expect("assembles");
        assert_eq!(
            totals.total_due,
            vnd(2_200),
            "the price and the fee, as quoted"
        );
        assert_eq!(totals.tax_total, vnd(200), "2,200 × 10/110");
        assert_eq!(fee_taxes(&totals), vec![vnd(100)], "the tax inside the fee");
        assert_reconciles(&totals);
        // A percentage of the tax-inclusive price is taken of the price as quoted.
        let on_quoted = [altered(fee(2, FeeKind::Percent, 1_000), |f| {
            f.base_tax_inclusive = true;
        })];
        let totals = assemble_with(&quoted, &on_quoted, &rates, inclusive).expect("assembles");
        assert_eq!(totals.service_charge, vnd(110));
        assert_eq!(totals.total_due, vnd(1_210));
        assert_eq!(
            fee_taxes(&totals),
            vec![vnd(10)],
            "1,210 holds 110, a tenth of it the fee's"
        );
        assert_reconciles(&totals);
    }

    #[test]
    fn the_parts_of_a_class_share_its_tax_exactly() {
        let rates = rates();
        let rules = [
            taxed_at(food(), fee(1, FeeKind::AmountPerBill, 7_777)),
            taxed_at(food(), fee(2, FeeKind::AmountPerBill, 3_333)),
        ];
        let with_service_charge = |input: &mut BillInput<'_>| input.service_charge = vnd(15_000);
        let totals =
            assemble_with(&dinner(), &rules, &rates, with_service_charge).expect("assembles");
        // Food's base is the lines' 200,000, the service charge's 15,000 and the fees' 7,777 and
        // 3,333: 226,110, taxed once at 10 % to 22,611. Each fee's part is its share of that,
        // floored from 777.7 and 333.3, and the service charge's is exactly 1,500, so the lines
        // take the 20,001 left: the four parts sum to the class's tax.
        assert_eq!(fee_taxes(&totals), vec![vnd(777), vnd(333)]);
        let food_tax = totals
            .tax_lines
            .iter()
            .find(|line| line.tax_class_id == food())
            .map(|line| (line.taxable_base, line.tax));
        assert_eq!(food_tax, Some((vnd(226_110), vnd(22_611))));
        assert_eq!(totals.tax_total, vnd(22_611 + 15_000));
        assert_reconciles(&totals);
    }

    #[test]
    fn a_class_taxed_only_for_its_fees_gives_them_its_whole_tax() {
        let rates = rates().with(service(), SalesChannel::DineIn, TaxRate::from_percent(8));
        let rules = [
            taxed_at(service(), fee(1, FeeKind::AmountPerBill, 3_333)),
            taxed_at(service(), fee(2, FeeKind::AmountPerBill, 3_334)),
            taxed_at(service(), fee(3, FeeKind::AmountPerBill, 0)),
        ];
        let totals = assemble_with(&dinner(), &rules, &rates, |_| {}).expect("assembles");
        // 6,667 at 8 % is 533.36, rounded once to 533. The first fee's part is 266.46, floored,
        // and the last fee with a part takes what is left; a fee of nothing carries nothing.
        assert_eq!(fee_taxes(&totals), vec![vnd(266), vnd(267), vnd(0)]);
        let service_tax = totals
            .tax_lines
            .iter()
            .find(|line| line.tax_class_id == service())
            .map(|line| line.tax);
        assert_eq!(service_tax, Some(vnd(533)));
        assert_reconciles(&totals);
    }

    /// What each fee charged on a bill, by rule, in đồng: zero for a rule that charged nothing.
    fn charged_by_rule(totals: &BillTotals, rules: &[FrozenFee]) -> Vec<i64> {
        rules
            .iter()
            .map(|rule| {
                totals
                    .fee_lines
                    .iter()
                    .filter(|line| line.fee_id == rule.fee_id)
                    .map(|line| line.amount.amount_minor)
                    .sum()
            })
            .collect()
    }

    #[test]
    fn a_split_shares_a_fee_per_bill_and_each_part_charges_the_rest_on_its_own_lines() {
        let rates = rates();
        let rules = [
            untaxed(fee(1, FeeKind::AmountPerBill, 10_000)),
            untaxed(fee(2, FeeKind::Percent, 1_000)),
            untaxed(only(
                FeeItems::Include,
                1,
                fee(3, FeeKind::AmountPerBill, 5_000),
            )),
            untaxed(only(
                FeeItems::Include,
                0,
                fee(4, FeeKind::AmountPerUnit, 3_000),
            )),
        ];
        let parts = [
            vec![line(0, 2, 200_000, food())],
            vec![line(1, 1, 100_000, alcohol())],
        ];
        let kept = split_fee_rules(&rules, &parts).expect("splits");
        let charged: Vec<Vec<i64>> = parts
            .iter()
            .zip(&kept)
            .map(|(lines, kept)| {
                let totals = assemble_with(lines, kept, &rates, |_| {}).expect("a part assembles");
                assert_reconciles(&totals);
                charged_by_rule(&totals, &rules)
            })
            .collect();
        // The 10,000 per bill falls 2:1 as the parts' nets do, and the 5,000 that counts only the
        // wine falls on the wine; the percentage and the boxes are charged on each part's own
        // lines. Together the parts charge what the whole bill does.
        assert_eq!(
            charged,
            vec![vec![6_666, 20_000, 0, 6_000], vec![3_334, 10_000, 5_000, 0]]
        );
        let whole = assemble_with(&dinner(), &rules, &rates, |_| {}).expect("assembles");
        assert_eq!(
            charged_by_rule(&whole, &rules),
            vec![10_000, 30_000, 5_000, 6_000]
        );
        // A part keeps a fee per bill only for its share: the pizzas have no wine to charge for.
        let ids: Vec<Vec<u128>> = kept
            .iter()
            .map(|rules| {
                rules
                    .iter()
                    .map(|rule| rule.fee_id.as_ulid().to_u128())
                    .collect()
            })
            .collect();
        assert_eq!(ids, vec![vec![1, 2, 4], vec![1, 2, 3, 4]]);
    }

    /// The amount of the fee per bill numbered `id` a merged bill holds, if it holds one.
    fn per_bill(merged: &MergedFees, id: u128) -> Option<Money> {
        merged
            .fee_rules
            .iter()
            .find(|rule| rule.fee_id == FeeId::new(Ulid::from_u128(id)))
            .and_then(|rule| rule.amount)
    }

    /// Merges bills holding `bills`' rules, the first being the holder, every fee per bill a share
    /// of the wholes in `wholes`.
    fn merged(wholes: &[FeeWhole], bills: &[&[FrozenFee]]) -> MergedFees {
        let mut merging = bills.iter().map(|fee_rules| MergingBill {
            fee_rules,
            fee_wholes: wholes,
            waived_fee_ids: &[],
        });
        let holder = merging.next().expect("a holder");
        let absorbed: Vec<MergingBill<'_>> = merging.collect();
        merge_fee_rules(holder, &absorbed).expect("merges")
    }

    #[test]
    fn a_merge_charges_a_fee_per_bill_once_and_puts_a_split_back_together() {
        let rules = [
            untaxed(fee(1, FeeKind::AmountPerBill, 10_000)),
            untaxed(fee(2, FeeKind::Percent, 1_000)),
        ];
        let wholes = fee_wholes(&rules);
        let thirds = [
            vec![line(0, 1, 100_000, food())],
            vec![line(0, 1, 100_000, food())],
            vec![line(0, 1, 100_000, food())],
        ];
        let kept = split_fee_rules(&rules, &thirds).expect("splits");
        let [first, second, third] = kept.as_slice() else {
            panic!("three parts: {kept:?}");
        };
        // 10,000 split three ways is 3,333, 3,333 and 3,334. Every part back together is the
        // whole again, and some of them the sum of their shares; the percentage is the holder's.
        let all = merged(&wholes, &[first, second, third]);
        assert_eq!(per_bill(&all, 1), Some(vnd(10_000)));
        assert_eq!(all.fee_rules.get(1), rules.get(1));
        assert_eq!(
            per_bill(&merged(&wholes, &[third, first]), 1),
            Some(vnd(6_667))
        );
        // Two bills split from no common bill charge it once, not once each.
        assert_eq!(
            per_bill(&merged(&wholes, &[&rules, &rules]), 1),
            Some(vnd(10_000))
        );

        // A cover on the food alone: the drinks' part drops it, its share being nothing, and
        // holding that part while absorbing the food's keeps the cover, after the drinks' rules.
        let food_cover = [
            untaxed(fee(2, FeeKind::Percent, 1_000)),
            only(
                FeeItems::Include,
                0,
                untaxed(fee(1, FeeKind::AmountPerBill, 10_000)),
            ),
        ];
        let parts = [
            vec![line(1, 1, 40_000, alcohol())],
            vec![line(0, 1, 100_000, food())],
        ];
        let kept = split_fee_rules(&food_cover, &parts).expect("splits");
        let [drinks, dishes] = kept.as_slice() else {
            panic!("two parts: {kept:?}");
        };
        assert_eq!(
            drinks.len(),
            1,
            "the drinks keep the percentage alone: {drinks:?}"
        );
        let held = merged(&fee_wholes(&food_cover), &[drinks, dishes]);
        assert_eq!(per_bill(&held, 1), Some(vnd(10_000)));
        let order: Vec<u128> = held
            .fee_rules
            .iter()
            .map(|rule| rule.fee_id.as_ulid().to_u128())
            .collect();
        assert_eq!(order, [2, 1]);
    }

    #[test]
    fn a_waived_fee_charges_and_taxes_nothing_and_leaves_the_rest_as_they_were() {
        // ADR-0159 decision 5. Waiving the service charge leaves the cover as it was, and the bill
        // is the bill that never froze a service charge at all.
        let rates = rates();
        let service = altered(fee(1, FeeKind::Percent, 1_000), |rule| rule.waivable = true);
        let cover = fee(2, FeeKind::AmountPerBill, 10_000);
        let rules = [service.clone(), cover.clone()];
        let without = assemble_waiving(&dinner(), &rules, &[service.fee_id], &rates, |_| {})
            .expect("assembles");
        let cover_alone = assemble_with(&dinner(), &[cover], &rates, |_| {}).expect("assembles");
        assert_eq!(without, cover_alone);
        assert_reconciles(&without);

        // Charged, the service charge says it may be waived, and the cover that it may not.
        let charged = assemble_with(&dinner(), &rules, &rates, |_| {}).expect("assembles");
        let waivable: Vec<bool> = charged.fee_lines.iter().map(|fee| fee.waivable).collect();
        assert_eq!(waivable, [true, false]);

        // Every fee waived is a bill that froze none.
        let every = [service.fee_id, FeeId::new(Ulid::from_u128(2))];
        let bare = assemble_waiving(&dinner(), &rules, &every, &rates, |_| {}).expect("assembles");
        assert!(bare.fee_lines.is_empty());
        assert_eq!(
            bare,
            assemble_with(&dinner(), &[], &rates, |_| {}).expect("assembles")
        );
    }

    #[test]
    fn only_a_waivable_fee_the_bill_charges_is_waived() {
        let service = altered(fee(1, FeeKind::Percent, 1_000), |rule| rule.waivable = true);
        let cover = fee(2, FeeKind::AmountPerBill, 10_000);
        let rules = [service.clone(), cover.clone()];
        assert_eq!(waivable_fee(&rules, &[], service.fee_id), Ok(&service));
        assert_eq!(
            waivable_fee(&rules, &[], cover.fee_id),
            Err(DomainError::FeeNotWaivable)
        );
        assert_eq!(
            waivable_fee(&rules, &[service.fee_id], service.fee_id),
            Err(DomainError::FeeNotOnBill),
            "waived already"
        );
        assert_eq!(
            waivable_fee(&rules, &[], FeeId::new(Ulid::from_u128(9))),
            Err(DomainError::FeeNotOnBill),
            "a rule the bill never froze"
        );
    }

    #[test]
    fn a_fee_waived_on_any_merged_bill_stays_waived_on_the_merge() {
        // ADR-0159 decision 5: a waive follows the fee's id through a merge, the holder's first.
        let rules = [
            fee(1, FeeKind::Percent, 1_000),
            fee(2, FeeKind::AmountPerBill, 10_000),
        ];
        let wholes = fee_wholes(&rules);
        let service = [FeeId::new(Ulid::from_u128(1))];
        let cover = [FeeId::new(Ulid::from_u128(2))];
        let bill = |waived_fee_ids| MergingBill {
            fee_rules: &rules,
            fee_wholes: &wholes,
            waived_fee_ids,
        };
        let merged = merge_fee_rules(bill(&cover), &[bill(&[]), bill(&service), bill(&cover)])
            .expect("merges");
        assert_eq!(
            merged.waived_fee_ids,
            [
                FeeId::new(Ulid::from_u128(2)),
                FeeId::new(Ulid::from_u128(1))
            ]
        );
        let none = merge_fee_rules(bill(&[]), &[bill(&[])]).expect("merges");
        assert!(none.waived_fee_ids.is_empty());
    }

    /// A rule from a property test's choices of kind, tax treatment, item scope and value.
    fn chosen(id: u128, kind: u8, tax: u8, scope: u8, value: i64) -> FrozenFee {
        let kind = match kind {
            0 => FeeKind::Percent,
            1 => FeeKind::AmountPerBill,
            _ => FeeKind::AmountPerUnit,
        };
        let fee = fee(id, kind, value);
        let fee = match tax {
            0 => untaxed(fee),
            1 => fee,
            2 => taxed_at(food(), fee),
            _ => taxed_at(service(), fee),
        };
        match scope {
            0 => fee,
            1 => only(FeeItems::Include, 0, fee),
            _ => only(FeeItems::Exclude, 0, fee),
        }
    }

    proptest! {
        // Data-correctness law §14.3, at the domain level: a split sums exactly to the original.
        #[test]
        fn an_even_split_sums_exactly(total in 0_i64..=1_000_000_000, parts in 1_usize..=50) {
            let split = split_evenly(vnd(total), parts).expect("splits");
            prop_assert_eq!(split.len(), parts);
            let mut sum = vnd(0);
            for part in &split {
                sum = sum.checked_add(*part).expect("in range");
            }
            prop_assert_eq!(sum, vnd(total), "sum(splits) == original_total");
        }

        #[test]
        fn a_weighted_split_sums_exactly(
            total in 0_i64..=1_000_000_000,
            weights in prop::collection::vec(1_i64..=1_000, 1..=20),
        ) {
            let split = split_by_weights(vnd(total), &weights).expect("splits");
            prop_assert_eq!(split.len(), weights.len());
            let mut sum = vnd(0);
            for part in &split {
                sum = sum.checked_add(*part).expect("in range");
            }
            prop_assert_eq!(sum, vnd(total), "a weighted split sums exactly too");
        }

        // The settlement change identity holds whenever the applied amounts sum to the total.
        #[test]
        fn the_change_identity_holds(
            total in 1_i64..=10_000_000,
            over in 0_i64..=1_000_000,
            tip in 0_i64..=1_000_000,
        ) {
            let payment = Payment {
                method: PaymentMethod::Cash,
                tendered: vnd(total + over + tip),
                applied_to_bill: vnd(total),
                tip: vnd(tip),
            };
            let settlement = settle(vnd(total), &[payment]).expect("settles");
            // tendered − applied − tip == change
            prop_assert_eq!(settlement.change_given, vnd(over));
        }

        /// The sum law with fees (ADR-0159 decision 2): whatever the rules, the total is the lines
        /// less their reductions, plus the service charge, the fees, the tax and the rounding; each
        /// fee's taxed parts sum to it; and every taxed part lands in exactly one class's base.
        ///
        /// And tax per fee: each class's tax, rounded once on that base, is shared out among its
        /// parts. A fee's part takes its share floored; the service charge's part and the lines'
        /// take the rest, which is their share to within a minor unit per fee part, or nothing
        /// where the fees are the class's whole base. So per class, the parts' taxes sum to the
        /// class's tax, and a fee's tax is its parts' taxes.
        #[test]
        fn a_bill_with_fees_still_reconciles(
            sold in prop::collection::vec(
                (0_u128..3, 1_i64..=4, 1_i64..=5_000_000, any::<bool>()),
                1..=6,
            ),
            discount_percent in 0_i64..=50,
            service_charge in prop_oneof![Just(0_i64), 1_i64..=50_000],
            choices in prop::collection::vec((0_u8..3, 0_u8..4, 0_u8..3, 0_i64..=20_000), 0..=3),
            half_even in any::<bool>(),
            cash_rounding in any::<bool>(),
        ) {
            let class = |is_food: bool| if is_food { food() } else { alcohol() };
            let lines: Vec<BillLine> = sold
                .iter()
                .map(|(n, units, net, is_food)| line(*n, *units, *net, class(*is_food)))
                .collect();
            let rules: Vec<FrozenFee> = (1..)
                .zip(&choices)
                .map(|(id, (kind, tax, scope, value))| chosen(id, *kind, *tax, *scope, *value))
                .collect();
            let sold_total: i64 = lines.iter().map(|line| line.net.amount_minor).sum();
            let discount = sold_total * discount_percent / 100;
            let rates = rates().with(service(), SalesChannel::DineIn, TaxRate::from_percent(8));
            let totals = assemble_with(&lines, &rules, &rates, |input| {
                input.bill_discount = vnd(discount);
                input.service_charge = vnd(service_charge);
                input.rounding_mode = if half_even { Rounding::HalfEven } else { Rounding::HalfUp };
                input.cash_rounding_increment = cash_rounding.then_some(500);
            })
            .expect("assembles");

            assert_reconciles(&totals);
            let fees: i64 = totals.fee_lines.iter().map(|fee| fee.amount.amount_minor).sum();
            prop_assert_eq!(totals.service_charge, vnd(service_charge + fees));
            let mut taxed = 0;
            for fee in &totals.fee_lines {
                let parts: i64 = fee.class_shares.iter().map(|part| part.amount.amount_minor).sum();
                prop_assert!(fee.class_shares.is_empty() || parts == fee.amount.amount_minor);
                let taxes: i64 = fee.class_shares.iter().map(|part| part.tax.amount_minor).sum();
                prop_assert_eq!(fee.tax, vnd(taxes), "a fee's tax is its parts' taxes");
                taxed += parts;
            }
            let bases: i64 = totals.tax_lines.iter().map(|tax| tax.taxable_base.amount_minor).sum();
            prop_assert_eq!(bases, sold_total - discount + service_charge + taxed);

            // Per class, in i128 so the products cannot overflow. Prices here exclude tax, so a
            // tax line's base is the whole of what its tax was charged on.
            let minor = |money: Money| i128::from(money.amount_minor);
            let mut placed = 0_i128;
            for tax_line in &totals.tax_lines {
                let parts: Vec<&FeeClassShare> = totals
                    .fee_lines
                    .iter()
                    .flat_map(|fee| &fee.class_shares)
                    .filter(|part| part.tax_class_id == tax_line.tax_class_id)
                    .collect();
                let (base, tax) = (minor(tax_line.taxable_base), minor(tax_line.tax));
                let fee_base: i128 = parts.iter().map(|part| minor(part.amount)).sum();
                let fee_tax: i128 = parts.iter().map(|part| minor(part.tax)).sum();
                placed += fee_base;
                for part in &parts {
                    prop_assert!(!part.amount.is_zero() || part.tax.is_zero(), "nothing, untaxed");
                }
                // The service charge's part and the lines', and the tax left to them.
                let rest = base - fee_base;
                let rest_tax = tax - fee_tax;
                if rest == 0 {
                    prop_assert_eq!(rest_tax, 0, "the fees are the base, so they take its tax");
                } else {
                    for part in &parts {
                        prop_assert_eq!(minor(part.tax), tax * minor(part.amount) / base);
                    }
                    let fee_parts = i128::try_from(parts.len()).expect("a few parts");
                    let over = rest_tax * base - tax * rest;
                    prop_assert!(over >= 0 && over <= fee_parts * (base - 1), "the rest's share");
                }
            }
            prop_assert_eq!(placed, i128::from(taxed), "every fee part is in its class's base");
        }

        /// ADR-0159 decision 5: a waived fee is the rule left off the bill, whatever the rules, the
        /// lines and the reductions, so it charges nothing, taxes nothing and moves no other fee.
        #[test]
        fn waiving_a_fee_is_assembling_the_bill_without_it(
            sold in prop::collection::vec(
                (0_u128..3, 1_i64..=4, 1_i64..=5_000_000, any::<bool>()),
                1..=6,
            ),
            discount_percent in 0_i64..=50,
            choices in prop::collection::vec((0_u8..3, 0_u8..4, 0_u8..3, 0_i64..=20_000), 0..=4),
            waiving in prop::collection::vec(any::<bool>(), 4),
        ) {
            let class = |is_food: bool| if is_food { food() } else { alcohol() };
            let lines: Vec<BillLine> = sold
                .iter()
                .map(|(n, units, net, is_food)| line(*n, *units, *net, class(*is_food)))
                .collect();
            let rules: Vec<FrozenFee> = (1..)
                .zip(&choices)
                .map(|(id, (kind, tax, scope, value))| chosen(id, *kind, *tax, *scope, *value))
                .collect();
            let waived: Vec<FeeId> = rules
                .iter()
                .zip(&waiving)
                .filter(|(_, waive)| **waive)
                .map(|(rule, _)| rule.fee_id)
                .collect();
            let kept: Vec<FrozenFee> = rules
                .iter()
                .filter(|rule| !waived.contains(&rule.fee_id))
                .cloned()
                .collect();
            let sold_total: i64 = lines.iter().map(|line| line.net.amount_minor).sum();
            let discount = vnd(sold_total * discount_percent / 100);
            let rates = rates().with(service(), SalesChannel::DineIn, TaxRate::from_percent(8));
            let with_waives = assemble_waiving(&lines, &rules, &waived, &rates, |input| {
                input.bill_discount = discount;
            })
            .expect("assembles");
            let without_rules = assemble_with(&lines, &kept, &rates, |input| {
                input.bill_discount = discount;
            })
            .expect("assembles");
            prop_assert_eq!(&with_waives, &without_rules);
            prop_assert!(
                with_waives.fee_lines.iter().all(|fee| !waived.contains(&fee.fee_id)),
                "a waived fee charges nothing"
            );
        }

        /// ADR-0159 decision 6: the parts of a split bill charge a fee per bill together exactly as
        /// the whole would have, and a percentage or a fee per unit on their own lines, so those
        /// come to the whole within a minor unit per part, each part rounding once. Merged back,
        /// any of the parts charge a fee per bill at the sum of their shares, whichever of them
        /// holds the rest, and all of them at the whole.
        #[test]
        fn the_parts_of_a_split_bill_charge_what_the_whole_would_have(
            sold in prop::collection::vec(
                (0_u128..3, 1_i64..=4, 0_i64..=5_000_000, any::<bool>(), 0_usize..3),
                1..=8,
            ),
            choices in prop::collection::vec((0_u8..3, 0_u8..4, 0_u8..3, 0_i64..=20_000), 0..=4),
            half_even in any::<bool>(),
            merging in prop::collection::vec(any::<bool>(), 3),
            holding in 0_usize..3,
        ) {
            let class = |is_food: bool| if is_food { food() } else { alcohol() };
            let mut lines = Vec::new();
            let mut parts: Vec<Vec<BillLine>> = vec![Vec::new(); 3];
            for (n, units, net, is_food, part) in &sold {
                let sold_line = line(*n, *units, *net, class(*is_food));
                lines.push(sold_line);
                if let Some(part) = parts.get_mut(*part) {
                    part.push(sold_line);
                }
            }
            parts.retain(|part| !part.is_empty());
            let rules: Vec<FrozenFee> = (1..)
                .zip(&choices)
                .map(|(id, (kind, tax, scope, value))| chosen(id, *kind, *tax, *scope, *value))
                .collect();
            let rates = rates().with(service(), SalesChannel::DineIn, TaxRate::from_percent(8));
            let mode = if half_even { Rounding::HalfEven } else { Rounding::HalfUp };
            let round = |input: &mut BillInput<'_>| input.rounding_mode = mode;

            let whole = assemble_with(&lines, &rules, &rates, round).expect("assembles");
            let kept = split_fee_rules(&rules, &parts).expect("splits");
            prop_assert_eq!(kept.len(), parts.len());
            let mut together = vec![0_i64; rules.len()];
            for (part, kept) in parts.iter().zip(&kept) {
                let totals = assemble_with(part, kept, &rates, round).expect("a part assembles");
                assert_reconciles(&totals);
                for (sum, charged) in together.iter_mut().zip(charged_by_rule(&totals, &rules)) {
                    *sum += charged;
                }
            }
            let per_part = i64::try_from(parts.len()).expect("a few parts");
            let wholes = charged_by_rule(&whole, &rules);
            for ((rule, whole), together) in rules.iter().zip(&wholes).zip(together) {
                if rule.kind() == Some(FeeKind::AmountPerBill) {
                    prop_assert_eq!(together, *whole, "a fee per bill is shared, never repeated");
                } else {
                    prop_assert!((together - whole).abs() <= per_part, "{} for {}", together, whole);
                }
            }

            // Some of the parts merged back, or all of them when none is picked.
            let mut picked: Vec<usize> = (0..parts.len())
                .filter(|index| merging.get(*index) == Some(&true))
                .collect();
            if picked.is_empty() {
                picked = (0..parts.len()).collect();
            }
            let holder = holding % picked.len();
            picked.rotate_left(holder);
            let part_wholes = fee_wholes(&rules);
            let held: Vec<&[FrozenFee]> = picked
                .iter()
                .map(|index| kept.get(*index).map_or(&[][..], Vec::as_slice))
                .collect();
            let merged_fees = merged(&part_wholes, &held);
            let merged_lines: Vec<BillLine> = picked
                .iter()
                .filter_map(|index| parts.get(*index))
                .flatten()
                .copied()
                .collect();
            let totals = assemble_with(&merged_lines, &merged_fees.fee_rules, &rates, round)
                .expect("the merged bill assembles");
            assert_reconciles(&totals);
            let charged = charged_by_rule(&totals, &rules);
            for ((rule, charged), whole) in rules.iter().zip(charged).zip(&wholes) {
                if rule.kind() != Some(FeeKind::AmountPerBill) {
                    continue;
                }
                let shares: i64 = held
                    .iter()
                    .filter_map(|rules| rules.iter().find(|kept| kept.fee_id == rule.fee_id))
                    .filter_map(|kept| kept.amount)
                    .map(|amount| amount.amount_minor)
                    .sum();
                prop_assert_eq!(charged, shares, "the merged parts' shares");
                prop_assert!(charged <= *whole, "never more than the whole");
                if picked.len() == parts.len() {
                    prop_assert_eq!(charged, *whole, "every part back together is the whole");
                }
            }
        }
    }
}
