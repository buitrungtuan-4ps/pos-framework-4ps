// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The billing tests that open with ADR-0104's two country facts that could not be
//! configuration (a tax printed as its components, and a price that includes its tax), then go on
//! to tax per class, the service charge, cash rounding, the refusals, and settling.

use super::*;

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
    assert_eq!(cgst.name.as_str(), "CGST");
    assert_eq!(cgst.tax, vnd(90));
    assert_eq!(sgst.name.as_str(), "SGST");
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
    let rates = TaxRateTable::new().with(food(), SalesChannel::DineIn, TaxRate::from_percent(10));
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
    let rates = TaxRateTable::new().with(food(), SalesChannel::DineIn, TaxRate::from_percent(10));
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
    let partial = TaxRateTable::new().with(food(), SalesChannel::DineIn, TaxRate::from_percent(10));
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
