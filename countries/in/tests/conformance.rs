// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! India's conformance pack: golden bills from this module's own values.
//!
//! Rates, posture and rounding come from [`India::locale_pack`]. Line prices are illustrative
//! inputs. Each case's arithmetic is worked in its comment. `CONFORMANCE.md` beside this crate
//! maps the requirements a human must confirm to these case numbers.

use core::num::NonZeroU32;

use pos_contract_tests::country_conformance::{Case, Expected, Line, TaxLine, failures, totals};
use pos_country::{CountryModule, tax_class};
use pos_country_in::{India, fiscalization};
use pos_fakes::executor::run_ready;
use pos_ports::fiscalization::Fiscalization;
use pos_proto::{SalesChannel, StoreId, Ulid};

fn line(tax_class_id: pos_proto::TaxClassId, net_minor: i64) -> Line {
    Line {
        tax_class_id,
        net_minor,
    }
}

fn gst(tax_class_id: pos_proto::TaxClassId, base: i64, rate: u32, tax: i64) -> TaxLine {
    TaxLine {
        tax_class_id,
        taxable_base_minor: base,
        rate_basis_points: rate,
        tax_minor: tax,
        components: Vec::new(),
    }
}

/// CGST and SGST at 2.5 % each, splitting `tax` as `Money::allocate` does: the first half is
/// rounded down and the second takes the residual.
fn halves(tax: i64) -> Vec<(&'static str, u32, i64)> {
    let cgst = tax / 2;
    vec![("CGST", 250, cgst), ("SGST", 250, tax - cgst)]
}

fn composed(base: i64, tax: i64) -> TaxLine {
    TaxLine {
        components: halves(tax),
        ..gst(tax_class::food(), base, 500, tax)
    }
}

fn cases() -> Vec<Case> {
    let (food, alcohol, exempt) = (tax_class::food(), tax_class::alcohol(), tax_class::exempt());
    vec![
        // MRP ₹525.00 at 5 %: tax = 52,500 × 5/105 = 2,500 paise, base 50,000.
        // CGST 1,250 + SGST 1,250. Already a whole rupee, so no rounding.
        Case {
            number: 1,
            title: "dine-in food: 5 % extracted from MRP, printed as CGST and SGST",
            cites: &["GST_RESTAURANT", "GST_HALF", "prices_include_tax: true"],
            channel: SalesChannel::DineIn,
            lines: vec![line(food, 52_500)],
            expected: totals(vec![composed(50_000, 2_500)], 50_000, 2_500, 0, 52_500),
        },
        // ₹499.50: 49,950 × 5/105 = 2,378.57, half up to 2,379; base 47,571.
        // CGST 1,189 + SGST 1,190 (the residual). ₹499.50 rounds half up to ₹500.00: +50.
        Case {
            number: 2,
            title: "takeaway: an odd tax splits exactly, and the total rounds to the rupee",
            cites: &["GST_RESTAURANT", "GST_HALF", "CASH_ROUNDING"],
            channel: SalesChannel::Takeaway,
            lines: vec![line(food, 49_950)],
            expected: totals(vec![composed(47_571, 2_379)], 47_571, 2_379, 50, 50_000),
        },
        // Food ₹300.00: 30,000 × 5/105 = 1,428.57 → 1,429, base 28,571, CGST 714 + SGST 715.
        // Exempt ₹50.00 at 0 %, with no breakdown. Pays ₹350.00.
        Case {
            number: 3,
            title: "delivery: an exempt line prints at 0 % with no breakdown",
            cites: &["GST_RESTAURANT", "rate_table: exempt", "CASH_ROUNDING"],
            channel: SalesChannel::Delivery,
            lines: vec![line(food, 30_000), line(exempt, 5_000)],
            expected: totals(
                vec![composed(28_571, 1_429), gst(exempt, 5_000, 0, 0)],
                33_571,
                1_429,
                0,
                35_000,
            ),
        },
        // ₹100.49: 10,049 × 5/105 = 478.52 → 479, base 9,570, CGST 239 + SGST 240.
        // ₹100.49 rounds half up to ₹100.00: −49.
        Case {
            number: 4,
            title: "QR: cash rounding to the rupee can go down",
            cites: &["GST_RESTAURANT", "CASH_ROUNDING", "currency_exponent: 2"],
            channel: SalesChannel::Qr,
            lines: vec![line(food, 10_049)],
            expected: totals(vec![composed(9_570, 479)], 9_570, 479, -49, 10_000),
        },
        // The module has no alcohol row on purpose: liquor is state excise and VAT, so a store
        // must publish its state's rate before it can sell one.
        Case {
            number: 5,
            title: "alcohol is refused until the store publishes its state's rate",
            cites: &["rate_table: no alcohol row"],
            channel: SalesChannel::DineIn,
            lines: vec![line(alcohol, 50_000)],
            expected: Expected::RefusedNoRate,
        },
    ]
}

#[test]
fn every_bill_comes_out_as_the_case_says() {
    let found = failures(&India.locale_pack(), &cases());
    assert!(found.is_empty(), "{found:#?}");
}

/// Case 6: the invoice number is at most sixteen characters of letters, digits, `/` and `-`, and
/// a second range never repeats the first (the module cites Rule 46(b)).
#[test]
fn case_6_invoice_numbers_fit_rule_46b_and_never_collide() {
    let fiscal = fiscalization();
    let store = StoreId::new(Ulid::from_u128(1));
    let two = NonZeroU32::new(2).expect("2 is not zero");
    let first = run_ready(fiscal.allocate_range(store, two)).expect("a range");
    let second = run_ready(fiscal.allocate_range(store, two)).expect("a second range");
    let numbers = |range: &pos_ports::fiscalization::InvoiceRange| -> Vec<String> {
        range
            .numbers
            .iter()
            .map(|number| number.as_str().to_owned())
            .collect()
    };
    assert_eq!(numbers(&first), ["INV/00/000000", "INV/00/000001"]);
    assert_eq!(numbers(&second), ["INV/01/000000", "INV/01/000001"]);
    for number in numbers(&first).iter().chain(&numbers(&second)) {
        assert!(number.len() <= 16, "{number} is longer than sixteen");
        assert!(
            number
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '/' || c == '-'),
            "{number} has a character Rule 46(b) does not allow"
        );
    }
}

/// Case 7: a buyer's GSTIN is checked for shape, fifteen characters with the PAN inside.
#[test]
fn case_7_a_gstin_is_checked_for_shape_only() {
    assert!(India.is_valid_tax_code("29ABCDE1234F1Z5"));
    assert!(!India.is_valid_tax_code("29ABCDE1234F1Z"));
    assert!(!India.is_valid_tax_code("29ABCDE1234F1Y5"));
}
