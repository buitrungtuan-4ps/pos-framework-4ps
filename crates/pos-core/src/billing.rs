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

use pos_proto::locale::TaxRateTable;
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
        let tax = taxable.mul_ratio(rate.as_ratio(), input.rounding_mode)?;
        tax_total = tax_total.checked_add(tax)?;
        tax_lines.push(TaxLine {
            tax_class_id: base.tax_class_id,
            taxable_base: taxable,
            rate_basis_points: rate.basis_points(),
            tax,
        });
    }

    // If the service charge is taxable but its class was not among the line classes, tax it on its
    // own line so the charge is not silently untaxed.
    if input.service_charge_taxable
        && !input.service_charge.is_zero()
        && let Some(sc_class) = input.service_charge_tax_class
        && !input
            .class_bases
            .iter()
            .any(|base| base.tax_class_id == sc_class)
    {
        let rate = input
            .rates
            .rate_for(sc_class, input.sales_channel)
            .ok_or_else(|| DomainError::TaxRateNotConfigured {
                tax_class_id: sc_class.to_string(),
                sales_channel: pos_proto::wire_enum::WireEnum::as_wire(input.sales_channel)
                    .to_owned(),
            })?;
        let tax = input
            .service_charge
            .mul_ratio(rate.as_ratio(), input.rounding_mode)?;
        tax_total = tax_total.checked_add(tax)?;
        tax_lines.push(TaxLine {
            tax_class_id: sc_class,
            taxable_base: input.service_charge,
            rate_basis_points: rate.basis_points(),
            tax,
        });
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
/// `tendered` is what the guest handed over; `applied_to_bill` is what was put against the total.
/// Change and over-tender live in the gap between them — the distinction ADR-0028 requires, because
/// one field cannot be both.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Payment {
    /// How it was paid.
    pub method: PaymentMethod,
    /// What the guest handed over.
    pub tendered: Money,
    /// What was applied to the bill.
    pub applied_to_bill: Money,
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
pub fn settle(
    total_due: Money,
    payments: &[Payment],
    tips: &[Money],
) -> Result<Settlement, DomainError> {
    if payments.is_empty() {
        return Err(DomainError::Empty { what: "payments" });
    }
    let currency = total_due.currency_code;

    let mut total_applied = Money::zero(currency);
    let mut total_tendered = Money::zero(currency);
    for payment in payments {
        total_applied = total_applied.checked_add(payment.applied_to_bill)?;
        total_tendered = total_tendered.checked_add(payment.tendered)?;
    }
    let mut total_tips = Money::zero(currency);
    for tip in tips {
        total_tips = total_tips.checked_add(*tip)?;
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
    use pos_proto::locale::{TaxRate, TaxRateTable};
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
        };
        let settlement = settle(vnd(110_000), &[payment], &[]).expect("settles");
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
        };
        let settlement = settle(vnd(110_000), &[payment], &[vnd(20_000)]).expect("settles");
        // change = 200k − 110k applied − 20k tip = 70k
        assert_eq!(settlement.change_given, vnd(70_000));
        assert_eq!(settlement.total_tips, vnd(20_000));
        assert_eq!(
            settlement.total_applied,
            vnd(110_000),
            "the tip is not in what was applied"
        );
    }

    #[test]
    fn several_methods_combine_on_one_bill() {
        let card = Payment {
            method: PaymentMethod::Card,
            tendered: vnd(60_000),
            applied_to_bill: vnd(60_000),
        };
        let cash = Payment {
            method: PaymentMethod::Cash,
            tendered: vnd(50_000),
            applied_to_bill: vnd(50_000),
        };
        let settlement = settle(vnd(110_000), &[card, cash], &[]).expect("settles");
        assert_eq!(settlement.total_applied, vnd(110_000));
        assert_eq!(settlement.change_given, vnd(0));
    }

    #[test]
    fn under_application_is_refused() {
        let payment = Payment {
            method: PaymentMethod::Card,
            tendered: vnd(100_000),
            applied_to_bill: vnd(100_000),
        };
        let result = settle(vnd(110_000), &[payment], &[]);
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
        };
        let result = settle(vnd(110_000), &[payment], &[vnd(5_000)]);
        assert!(matches!(result, Err(DomainError::NegativeChange)));
    }

    #[test]
    fn settling_nothing_is_refused() {
        assert!(matches!(
            settle(vnd(110_000), &[], &[]),
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
            };
            let settlement = settle(vnd(total), &[payment], &[vnd(tip)]).expect("settles");
            // tendered − applied − tip == change
            prop_assert_eq!(settlement.change_given, vnd(over));
        }
    }
}
