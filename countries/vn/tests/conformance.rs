// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! Vietnam's conformance pack: golden bills from this module's own values.
//!
//! Rates, posture and rounding come from [`Vietnam::locale_pack`]. Line prices are illustrative
//! inputs. Each case's arithmetic is worked in its comment. `CONFORMANCE.md` beside this crate
//! maps the requirements a human must confirm to these case numbers.

use core::num::NonZeroU32;

use pos_contract_tests::country_conformance::{
    Case, Line, TaxLine, comp_failure, failures, totals,
};
use pos_country::{CountryModule, SettingDefault, SettingDefaultValue, tax_class};
use pos_country_vn::einvoice::{
    InvoiceKind, InvoiceSymbol, NumberingError, RECOMMENDED_ISSUE_MOMENT, RateTotal, TotalsError,
    check_totals, invoice_number, next_number,
};
use pos_country_vn::{Vietnam, fiscalization};
use pos_fakes::executor::run_ready;
use pos_ports::fiscalization::Fiscalization;
use pos_ports::fiscalization::InvoiceNumber;
use pos_proto::CalendarDate;
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

/// Case 8: a comp lowers the taxable base, as a discount does (ADR-0228 decision 1).
///
/// 200,000 of food with 50,000 comped: VAT on 150,000 is 15,000, so the guest owes 165,000, already
/// a whole thousand. Whether a giveaway is taxed at market value instead is to confirm by the
/// accountant (`CONFORMANCE.md` R21); this case is what changes if they say so.
#[test]
fn case_8_a_comp_lowers_the_taxable_base() {
    let food = tax_class::food();
    let case = Case {
        number: 8,
        title: "dine-in: a comp is taken off before VAT",
        cites: &["VAT_STANDARD", "ADR-0228 decision 1"],
        channel: SalesChannel::DineIn,
        lines: vec![line(food, 200_000)],
        expected: totals(
            vec![vat(food, 150_000, 1_000, 15_000)],
            200_000,
            15_000,
            0,
            165_000,
        ),
    };
    let found = comp_failure(&Vietnam.locale_pack(), &case, 50_000);
    assert!(found.is_none(), "{found:?}");
}

/// Case 9: no scale-label layout is seeded, so a 20–29 code reads as a plain code until a store
/// chooses one (ADR-0228 decision 2).
#[test]
fn case_9_no_variable_measure_layout_is_seeded() {
    assert!(
        Vietnam
            .setting_defaults()
            .iter()
            .all(|seeded| seeded.setting_key != "scan.variable_measure_layout"),
        "Vietnam's GS1 layout is to confirm by legal before the pack seeds one"
    );
}

/// Case 10: a weight typed by hand is forbidden, as law (ADR-0228 decision 3, ADR-0223).
#[test]
fn case_10_a_weight_typed_by_hand_is_forbidden() {
    assert!(Vietnam.forbidden_settings().contains(&SettingDefault {
        setting_key: "weighing.manual_entry",
        value: SettingDefaultValue::Bool(true),
    }));
}

/// Case 11: an e-invoice sequence runs gapless under its issuing point's symbol and restarts under
/// the next year's (ADR-0225 decision 2).
#[test]
fn case_11_an_invoice_sequence_is_gapless_and_restarts_with_the_year() {
    let store = InvoiceSymbol::for_issuing_point(1, 'C', 2026, 'M', 0).expect("1C26MAA");
    let december = CalendarDate::from_ymd(2026, 12, 31).expect("a date");
    let mut last = None;
    let mut numbers = Vec::new();
    for _ in 0..3 {
        let next = next_number(&store, last, december).expect("a number");
        numbers.push(invoice_number(&store, next));
        last = Some(next);
    }
    let printed: Vec<&str> = numbers.iter().map(InvoiceNumber::as_str).collect();
    assert_eq!(
        printed,
        ["1C26MAA/00000001", "1C26MAA/00000002", "1C26MAA/00000003"]
    );
    let january = CalendarDate::from_ymd(2027, 1, 1).expect("a date");
    assert_eq!(
        next_number(&store, last, january),
        Err(NumberingError::WrongYear)
    );
    let next_year = InvoiceSymbol::for_issuing_point(1, 'C', 2027, 'M', 0).expect("1C27MAA");
    assert_eq!(next_number(&next_year, None, january), Ok(1));
}

/// Case 12: a replacement box numbers under its own symbol, so its first invoice never repeats the
/// old box's, synced or not (ADR-0225 decision 2).
#[test]
fn case_12_a_replacement_box_never_repeats_a_number() {
    let on = CalendarDate::from_ymd(2026, 11, 24).expect("a date");
    let old_box = InvoiceSymbol::for_issuing_point(1, 'C', 2026, 'M', 0).expect("AA");
    let new_box = InvoiceSymbol::for_issuing_point(1, 'C', 2026, 'M', 1).expect("AB");
    let old_first = invoice_number(&old_box, next_number(&old_box, None, on).expect("1"));
    let new_first = invoice_number(&new_box, next_number(&new_box, None, on).expect("1"));
    assert_ne!(old_first, new_first);
}

/// Case 13: case 2's bill as an invoice: food and alcohol are both 10 %, so the invoice states the
/// rate once, 165,000 + 16,500 and the 500 rounding make 182,000 (ADR-0225 decision 1).
#[test]
fn case_13_an_invoice_states_each_rate_once_and_adds_up_to_the_bill() {
    let ten = RateTotal {
        rate_basis_points: 1_000,
        taxable_base_minor: 165_000,
        tax_minor: 16_500,
    };
    assert_eq!(
        check_totals(&InvoiceKind::Original, &[ten], 500, 182_000),
        Ok(())
    );
    let split = [
        RateTotal {
            taxable_base_minor: 120_000,
            tax_minor: 12_000,
            ..ten
        },
        RateTotal {
            taxable_base_minor: 45_000,
            tax_minor: 4_500,
            ..ten
        },
    ];
    assert_eq!(
        check_totals(&InvoiceKind::Original, &split, 500, 182_000),
        Err(TotalsError::RepeatedRate),
        "one line per class is the bill's; the invoice gives one per rate"
    );
}

/// Case 14: the recommended issue moment is every sale, buyer or not (ADR-0225 decision 3), an open
/// owner decision.
#[test]
fn case_14_every_sale_issues_an_invoice_by_default() {
    assert!(RECOMMENDED_ISSUE_MOMENT.issues(false));
    assert!(RECOMMENDED_ISSUE_MOMENT.issues(true));
}
