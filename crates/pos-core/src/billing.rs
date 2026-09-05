// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! Bill totals, tax, and settlement — the arithmetic ADR-0028 pins down.
//!
//! Everything here is a pure function over `pos-proto` value types: no clock, no I/O, no state.
//! Given the lines' pre-tax bases per tax class, the bill-level reductions, the service charge and
//! the store's rate table, [`assemble`] produces a [`BillTotals`] whose components reconcile to the
//! total exactly; given a total and a set of payments, [`settle`] proves the settlement invariant.
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

use pos_proto::locale::{TaxComponent, TaxRateTable};
use pos_proto::money::{Money, Rounding};
use pos_proto::{CurrencyCode, PaymentMethod, SalesChannel, TaxClassId};

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
    /// is taxable and belongs to this class.
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
    /// The service charge.
    pub service_charge: Money,
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

/// The service charge's own tax line, when it is taxable and its class was not among the bill's.
///
/// Without this the charge would be silently untaxed — it is added after discounts and before tax,
/// so a class nobody ordered from carries it. `None` means the charge is untaxed, zero, or already
/// folded into a class the bill has, all of which are handled where the classes are.
///
/// # Errors
///
/// [`DomainError::TaxRateNotConfigured`] if the charge's class has no rate on this channel, and
/// [`DomainError::Money`] on overflow — the same refusals the per-class loop makes, for the same
/// reason: a charge taxed at no rate is a tax-audit finding, not a default.
fn service_charge_line(input: &BillInput<'_>) -> Result<Option<TaxLine>, DomainError> {
    if !input.service_charge_taxable || input.service_charge.is_zero() {
        return Ok(None);
    }
    let Some(sc_class) = input.service_charge_tax_class else {
        return Ok(None);
    };
    if input
        .class_bases
        .iter()
        .any(|base| base.tax_class_id == sc_class)
    {
        return Ok(None);
    }

    let rate = input
        .rates
        .rate_for(sc_class, input.sales_channel)
        .ok_or_else(|| DomainError::TaxRateNotConfigured {
            tax_class_id: sc_class.to_string(),
            sales_channel: pos_proto::wire_enum::WireEnum::as_wire(input.sales_channel).to_owned(),
        })?;
    let (tax, taxable_base, components) = tax_for(
        input.service_charge,
        rate,
        input.rates.components_for(sc_class, input.sales_channel),
        input.prices_include_tax,
        input.rounding_mode,
    )?;
    Ok(Some(TaxLine {
        tax_class_id: sc_class,
        taxable_base,
        rate_basis_points: rate.basis_points(),
        tax,
        components,
    }))
}

/// Assembles a bill's totals, computing tax per class and materialising cash rounding.
///
/// # Errors
///
/// - [`DomainError::Empty`] if `class_bases` is empty.
/// - [`DomainError::Money`] on any currency mismatch or overflow.
/// - [`DomainError::TaxRateNotConfigured`] if a class has no rate on this channel — the domain
///   refuses rather than silently charging no tax.
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

    // Per-class taxable base = base − discount share − comp share (+ service charge if taxable here).
    let mut tax_lines = Vec::with_capacity(input.class_bases.len());
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

        if input.service_charge_taxable && input.service_charge_tax_class == Some(base.tax_class_id)
        {
            taxable = taxable.checked_add(input.service_charge)?;
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

    // The service charge may need a tax line of its own; see `service_charge_line`.
    if let Some(line) = service_charge_line(input)? {
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

    // Grand total before cash rounding.
    let unrounded = subtotal
        .checked_sub(input.bill_discount)?
        .checked_sub(input.comps)?
        .checked_add(input.service_charge)?
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
        service_charge: input.service_charge,
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
    use super::{BillInput, ClassBase, Payment, assemble, settle, split_by_weights, split_evenly};
    use crate::error::DomainError;
    use pos_proto::locale::{TaxComponent, TaxRate, TaxRateTable};
    use pos_proto::money::{Money, Rounding};
    use pos_proto::{CurrencyCode, PaymentMethod, SalesChannel, TaxClassId, Ulid};
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
        }
    }

    /// Every component reconciles to the total. This is the arithmetic ADR-0028 fixes, and the
    /// property a VAT invoice depends on.
    fn assert_reconciles(totals: &super::BillTotals) {
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
    }
}
