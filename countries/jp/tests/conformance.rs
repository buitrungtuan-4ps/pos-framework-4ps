// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! Japan's conformance pack: golden bills from this module's own values.
//!
//! Rates, posture and rounding come from [`Japan::locale_pack`]. Line prices are illustrative
//! inputs. Each case's arithmetic is worked in its comment. `CONFORMANCE.md` beside this crate
//! maps the requirements a human must confirm to these case numbers.

use core::num::NonZeroU32;

use pos_contract_tests::country_conformance::{
    Case, Line, TaxLine, comp_failure, failures_with_scope, totals,
};
use pos_country::{CountryModule, SettingDefault, SettingDefaultValue, tax_class};
use pos_country_jp::{Japan, fiscalization};
use pos_fakes::executor::run_ready;
use pos_ports::fiscalization::Fiscalization;
use pos_proto::{SalesChannel, StoreId, Ulid};

fn line(tax_class_id: pos_proto::TaxClassId, net_minor: i64) -> Line {
    Line {
        tax_class_id,
        net_minor,
    }
}

fn tax(tax_class_id: pos_proto::TaxClassId, base: i64, rate: u32, tax: i64) -> TaxLine {
    TaxLine {
        tax_class_id,
        taxable_base_minor: base,
        rate_basis_points: rate,
        tax_minor: tax,
        components: Vec::new(),
    }
}

fn cases() -> Vec<Case> {
    let (food, alcohol) = (tax_class::food(), tax_class::alcohol());
    vec![
        // 税込 ¥1,100 at 10 %: tax = 1,100 × 10/110 = 100, base 1,000. The guest pays ¥1,100.
        Case {
            number: 1,
            title: "dine-in food: 10 %, extracted from a 税込 price",
            cites: &[
                "CONSUMPTION_STANDARD",
                "prices_include_tax: true",
                "cash_rounding_increment: None",
            ],
            channel: SalesChannel::DineIn,
            lines: vec![line(food, 1_100)],
            expected: totals(vec![tax(food, 1_000, 1_000, 100)], 1_000, 100, 0, 1_100),
        },
        // ¥1,080 at 8 %: tax = 1,080 × 8/108 = 80, base 1,000.
        Case {
            number: 2,
            title: "takeaway food: the reduced 8 %",
            cites: &[
                "CONSUMPTION_REDUCED",
                "rate_table: carried_out",
                "prices_include_tax: true",
            ],
            channel: SalesChannel::Takeaway,
            lines: vec![line(food, 1_080)],
            expected: totals(vec![tax(food, 1_000, 800, 80)], 1_000, 80, 0, 1_080),
        },
        // Food ¥540 at 8 % → 40, base 500. Alcohol ¥330 at 10 % → 30, base 300. Pays ¥870.
        Case {
            number: 3,
            title: "takeaway: alcohol stays at 10 % beside 8 % food, one line per rate",
            cites: &[
                "CONSUMPTION_REDUCED",
                "CONSUMPTION_STANDARD",
                "rate_table: alcohol",
            ],
            channel: SalesChannel::Takeaway,
            lines: vec![line(food, 540), line(alcohol, 330)],
            expected: totals(
                vec![tax(food, 500, 800, 40), tax(alcohol, 300, 1_000, 30)],
                800,
                70,
                0,
                870,
            ),
        },
        // QR is a guest at a table, so dine-in: the same figures as case 1.
        Case {
            number: 4,
            title: "QR ordering takes the dine-in rate",
            cites: &["CONSUMPTION_STANDARD", "rate_table: eat_in"],
            channel: SalesChannel::Qr,
            lines: vec![line(food, 1_100)],
            expected: totals(vec![tax(food, 1_000, 1_000, 100)], 1_000, 100, 0, 1_100),
        },
        // ¥1,000 at 8 %: 1,000 × 8/108 = 74.07, half up to 74, base 926.
        Case {
            number: 5,
            title: "API (marketplace) is delivery, so 8 %, and a fraction of a yen rounds",
            cites: &[
                "CONSUMPTION_REDUCED",
                "rate_table: carried_out",
                "currency_exponent: 0",
            ],
            channel: SalesChannel::Api,
            lines: vec![line(food, 1_000)],
            expected: totals(vec![tax(food, 926, 800, 74)], 926, 74, 0, 1_000),
        },
        // Food ¥1,005 and alcohol ¥1,005, both 10 %. A qualified invoice rounds once per rate
        // (ADR-0226 decision 1): 2,010 × 10/110 = 182.73 → 183, not 91 + 91 = 182 from rounding
        // each class. The ¥183 is shared back by what each class contributed, half each, the odd
        // yen on the last class: 91 and 92, on bases of 914 and 913.
        Case {
            number: 6,
            title: "two classes at one rate round once, as one 10 % amount",
            cites: &[
                "CONSUMPTION_STANDARD",
                "rate_table: alcohol",
                "prices_include_tax: true",
                "tax_rounding_scope: PerRate",
            ],
            channel: SalesChannel::DineIn,
            lines: vec![line(food, 1_005), line(alcohol, 1_005)],
            expected: totals(
                vec![tax(food, 914, 1_000, 91), tax(alcohol, 913, 1_000, 92)],
                1_827,
                183,
                0,
                2_010,
            ),
        },
    ]
}

#[test]
fn every_bill_comes_out_as_the_case_says() {
    let found = failures_with_scope(&Japan.locale_pack(), Japan.tax_rounding_scope(), &cases());
    assert!(found.is_empty(), "{found:#?}");
}

/// Case 7: the invoice number is the seller's own serial, and a second range never repeats it.
#[test]
fn case_7_invoice_numbers_carry_the_series_and_never_collide() {
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
    assert_eq!(numbers(&first), ["0000-000000", "0000-000001"]);
    assert_eq!(numbers(&second), ["0001-000000", "0001-000001"]);
}

/// Case 8: a 登録番号 is checked for shape, `T` and thirteen digits, and nothing else.
#[test]
fn case_8_a_registration_number_is_checked_for_shape_only() {
    assert!(Japan.is_valid_tax_code("T1234567890123"));
    assert!(!Japan.is_valid_tax_code("1234567890123"));
    assert!(!Japan.is_valid_tax_code("T123456789012"));
}

/// Case 9: a comp lowers the taxable base, as a discount does (ADR-0228 decision 1).
///
/// 税込 ¥1,100 of dine-in food with ¥110 comped: ¥990 remains, whose 10 % is 990 × 10/110 = ¥90 on
/// a base of ¥900. Whether a giveaway is taxed at market value instead is to confirm by the
/// accountant.
#[test]
fn case_9_a_comp_lowers_the_taxable_base() {
    let food = tax_class::food();
    let case = Case {
        number: 9,
        title: "dine-in: a comp is taken off before the tax is extracted",
        cites: &["CONSUMPTION_STANDARD", "ADR-0228 decision 1"],
        channel: SalesChannel::DineIn,
        lines: vec![line(food, 1_100)],
        // 小計 is the quoted ¥1,100 net of the ¥90 tax, so 1,010 − 110 + 90 = ¥990 (ADR-0104).
        expected: totals(vec![tax(food, 900, 1_000, 90)], 1_010, 90, 0, 990),
    };
    let found = comp_failure(&Japan.locale_pack(), &case, 110);
    assert!(found.is_none(), "{found:?}");
}

/// Case 10: no scale-label layout is seeded, so a 20–29 code reads as a plain code until a store
/// chooses one (ADR-0228 decision 2).
#[test]
fn case_10_no_variable_measure_layout_is_seeded() {
    assert!(
        Japan
            .setting_defaults()
            .iter()
            .all(|seeded| seeded.setting_key != "scan.variable_measure_layout"),
        "the national GS1 layout is to confirm by legal before the pack seeds one"
    );
}

/// Case 11: a weight typed by hand is forbidden, as law (ADR-0228 decision 3, ADR-0223).
#[test]
fn case_11_a_weight_typed_by_hand_is_forbidden() {
    assert!(Japan.forbidden_settings().contains(&SettingDefault {
        setting_key: "weighing.manual_entry",
        value: SettingDefaultValue::Bool(true),
    }));
}
