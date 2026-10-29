// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! Vietnam's conformance pack: golden bills from this module's own values.
//!
//! Rates, posture and rounding come from [`Vietnam::locale_pack`]. Line prices are illustrative
//! inputs. Each case's arithmetic is worked in its comment. `CONFORMANCE.md` beside this crate
//! maps the requirements a human must confirm to these case numbers.

use core::num::NonZeroU32;

use pos_contract_tests::country_conformance::{Case, Line, TaxLine, failures, totals};
use pos_country::{CountryModule, tax_class};
use pos_country_vn::{Vietnam, fiscalization};
use pos_fakes::executor::run_ready;
use pos_ports::fiscalization::Fiscalization;
use pos_proto::{SalesChannel, StoreId, Ulid};

fn line(tax_class_id: pos_proto::TaxClassId, net_minor: i64) -> Line {
    Line {
        tax_class_id,
        net_minor,
    }
}

fn vat(tax_class_id: pos_proto::TaxClassId, base: i64, rate: u32, tax: i64) -> TaxLine {
    TaxLine {
        tax_class_id,
        taxable_base_minor: base,
        rate_basis_points: rate,
        tax_minor: tax,
        components: Vec::new(),
    }
}

fn cases() -> Vec<Case> {
    let (food, alcohol, exempt) = (tax_class::food(), tax_class::alcohol(), tax_class::exempt());
    vec![
        // 185,000 + 10 % = 203,500, which rounds half up to 204,000: +500.
        Case {
            number: 1,
            title: "dine-in food: VAT added on top, the total rounds to the thousand",
            cites: &["VAT_STANDARD", "prices_include_tax: false", "CASH_ROUNDING"],
            channel: SalesChannel::DineIn,
            lines: vec![line(food, 185_000)],
            expected: totals(
                vec![vat(food, 185_000, 1_000, 18_500)],
                185_000,
                18_500,
                500,
                204_000,
            ),
        },
        // Food 120,000 → 12,000; alcohol 45,000 → 4,500. 165,000 + 16,500 = 181,500 → 182,000.
        Case {
            number: 2,
            title: "takeaway: alcohol takes the same VAT as food, one line per class",
            cites: &["VAT_STANDARD", "rate_table: alcohol", "CASH_ROUNDING"],
            channel: SalesChannel::Takeaway,
            lines: vec![line(food, 120_000), line(alcohol, 45_000)],
            expected: totals(
                vec![
                    vat(food, 120_000, 1_000, 12_000),
                    vat(alcohol, 45_000, 1_000, 4_500),
                ],
                165_000,
                16_500,
                500,
                182_000,
            ),
        },
        // Food 99,000 → 9,900; exempt 10,000 → 0. 109,000 + 9,900 = 118,900 → 119,000.
        Case {
            number: 3,
            title: "delivery: an exempt line prints at 0 % beside the food",
            cites: &["VAT_STANDARD", "rate_table: exempt", "CASH_ROUNDING"],
            channel: SalesChannel::Delivery,
            lines: vec![line(food, 99_000), line(exempt, 10_000)],
            expected: totals(
                vec![vat(food, 99_000, 1_000, 9_900), vat(exempt, 10_000, 0, 0)],
                109_000,
                9_900,
                100,
                119_000,
            ),
        },
        // 92,000 + 9,200 = 101,200, which rounds half up to 101,000: −200.
        Case {
            number: 4,
            title: "QR: the same rate, and cash rounding can go down",
            cites: &["VAT_STANDARD", "CASH_ROUNDING"],
            channel: SalesChannel::Qr,
            lines: vec![line(food, 92_000)],
            expected: totals(
                vec![vat(food, 92_000, 1_000, 9_200)],
                92_000,
                9_200,
                -200,
                101_000,
            ),
        },
        // 12,345 × 10 % = 1,234.5, half up to 1,235. 13,580 rounds to 14,000: +420.
        Case {
            number: 5,
            title: "API: a half đồng of tax rounds up, once for the class",
            cites: &["VAT_STANDARD", "currency_exponent: 0", "CASH_ROUNDING"],
            channel: SalesChannel::Api,
            lines: vec![line(food, 12_345)],
            expected: totals(
                vec![vat(food, 12_345, 1_000, 1_235)],
                12_345,
                1_235,
                420,
                14_000,
            ),
        },
    ]
}

#[test]
fn every_bill_comes_out_as_the_case_says() {
    let found = failures(&Vietnam.locale_pack(), &cases());
    assert!(found.is_empty(), "{found:#?}");
}

/// Case 6: the invoice number has the module's shape, and a second range never repeats the first.
#[test]
fn case_6_invoice_numbers_carry_the_series_and_never_collide() {
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
    assert_eq!(numbers(&first), ["1C26T00/00000000", "1C26T00/00000001"]);
    assert_eq!(numbers(&second), ["1C26T01/00000000", "1C26T01/00000001"]);
}

/// Case 7: a buyer's mã số thuế is checked for shape, ten digits or ten and a branch.
#[test]
fn case_7_a_buyer_tax_code_is_checked_for_shape_only() {
    assert!(Vietnam.is_valid_tax_code("0101243150"));
    assert!(Vietnam.is_valid_tax_code("0101243150-001"));
    assert!(!Vietnam.is_valid_tax_code("010124315"));
    assert!(!Vietnam.is_valid_tax_code("0101243150-01"));
}
