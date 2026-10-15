// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The shift reports on paper, and a Vietnamese store's bill and drawer in Vietnamese.

use pos_ports::printer::PrintBlock;
use pos_proto::money::{CurrencyCode, Money};
use pos_proto::store_profile::StoreProfile;
use pos_proto::text::DisplayName;
use pos_proto::ulid::Ulid;

use crate::app::{CombinedReport, DrawerFigures, ShiftReport};
use crate::paper_labels::{ENGLISH, VIETNAMESE};
use crate::printing::tests::receipts::sold;
use crate::printing::tests::{lines_of, style, totals_of};
use crate::printing::{combined_shift_report_document, pre_bill_document, shift_report_document};

#[test]
fn a_shift_report_adds_up_on_paper_and_says_over_or_short_in_words() {
    // The float plus the cash taken is the expectation, and the count less the expectation is
    // the variance — in that order, so a supervisor checks it top to bottom. The word beside
    // the variance is what they read: a minus sign on thermal paper is one dot wide.
    let vnd = |minor| Money::new(CurrencyCode::VND, minor);
    let report = |counted: i64| ShiftReport {
        shift_id: pos_proto::ids::ShiftId::new(Ulid::from_u128(7)),
        opening_float: vnd(500_000),
        cash_collected: vnd(1_250_000),
        paid_in: vnd(0),
        paid_out: vnd(0),
        expected_amount: vnd(1_750_000),
        counted_amount: vnd(counted),
        variance: vnd(counted - 1_750_000),
        tax_components: Vec::new(),
    };

    let short = shift_report_document(
        &StoreProfile::default(),
        &style(0),
        &ENGLISH,
        &report(1_730_000),
    );
    let lines = lines_of(&short);
    assert_eq!(lines[0], "SHIFT REPORT");
    assert_eq!(
        lines[2..],
        [
            "Opening float  VND 500,000",
            "Cash taken  VND 1,250,000",
            "Expected in drawer  VND 1,750,000",
            "Counted  VND 1,730,000",
            "Variance  VND -20,000",
            "Short",
        ]
    );
    assert_eq!(short.blocks.last(), Some(&PrintBlock::Cut));

    let balanced = shift_report_document(
        &StoreProfile::default(),
        &style(0),
        &ENGLISH,
        &report(1_750_000),
    );
    assert_eq!(lines_of(&balanced).last(), Some(&"Balanced"));
    let over = shift_report_document(
        &StoreProfile::default(),
        &style(0),
        &ENGLISH,
        &report(1_760_000),
    );
    assert_eq!(lines_of(&over).last(), Some(&"Over"));
}

#[test]
fn a_combined_slip_prints_the_store_totals_then_each_drawer_under_its_tills_name() {
    let vnd = |minor| Money::new(CurrencyCode::VND, minor);
    let drawer = |seed: u128, float: i64, counted: i64| ShiftReport {
        shift_id: pos_proto::ids::ShiftId::new(Ulid::from_u128(seed)),
        opening_float: vnd(float),
        cash_collected: vnd(100_000),
        paid_in: vnd(0),
        paid_out: vnd(0),
        expected_amount: vnd(float + 100_000),
        counted_amount: vnd(counted),
        variance: vnd(counted - float - 100_000),
        tax_components: Vec::new(),
    };
    let bar = drawer(0xBA, 500_000, 590_000);
    let counter = drawer(0xC0, 300_000, 400_000);
    let combined = CombinedReport {
        totals: DrawerFigures {
            opening_float: vnd(800_000),
            cash_collected: vnd(200_000),
            paid_in: vnd(0),
            paid_out: vnd(0),
            expected_amount: vnd(1_000_000),
            counted_amount: vnd(990_000),
            variance: vnd(-10_000),
        },
        tax_components: Vec::new(),
        drawers: vec![
            (Some(DisplayName::new("Bar")), bar),
            (Some(DisplayName::new("Counter")), counter),
        ],
    };
    let slip =
        combined_shift_report_document(&StoreProfile::default(), &style(0), &ENGLISH, &combined);
    let figures = |float: &'static str, expected: &'static str, counted: &'static str| {
        [float, "Cash taken  VND 100,000", expected, counted]
    };
    let lines = lines_of(&slip);
    assert_eq!(lines[..2], ["SHIFT REPORT", "STORE TOTALS"]);
    assert_eq!(
        lines[2..9],
        [
            "Opening float  VND 800,000",
            "Cash taken  VND 200,000",
            "Expected in drawer  VND 1,000,000",
            "Counted  VND 990,000",
            "Variance  VND -10,000",
            "Short",
            "Bar",
        ]
    );
    assert_eq!(
        lines[10..14],
        figures(
            "Opening float  VND 500,000",
            "Expected in drawer  VND 600,000",
            "Counted  VND 590,000"
        )
    );
    assert_eq!(lines[16], "Counter");
    assert_eq!(lines.last(), Some(&"Balanced"));
    assert_eq!(slip.blocks.last(), Some(&PrintBlock::Cut));
}

#[test]
fn a_shift_report_prints_what_was_paid_in_and_out_so_the_expectation_adds_up() {
    // ADR-0165 decision 2: 500k float + 1,250k taken + 200k paid in - 150k paid out is 1,800k,
    // and the paper shows every term, in the order the sum runs.
    let vnd = |minor| Money::new(CurrencyCode::VND, minor);
    let report = ShiftReport {
        shift_id: pos_proto::ids::ShiftId::new(Ulid::from_u128(7)),
        opening_float: vnd(500_000),
        cash_collected: vnd(1_250_000),
        paid_in: vnd(200_000),
        paid_out: vnd(150_000),
        expected_amount: vnd(1_800_000),
        counted_amount: vnd(1_800_000),
        variance: vnd(0),
        tax_components: Vec::new(),
    };
    let english = shift_report_document(&StoreProfile::default(), &style(0), &ENGLISH, &report);
    assert_eq!(
        lines_of(&english)[2..],
        [
            "Opening float  VND 500,000",
            "Cash taken  VND 1,250,000",
            "Paid in  VND 200,000",
            "Paid out  VND 150,000",
            "Expected in drawer  VND 1,800,000",
            "Counted  VND 1,800,000",
            "Variance  VND 0",
            "Balanced",
        ]
    );

    let paid_out_only = ShiftReport {
        paid_in: vnd(0),
        expected_amount: vnd(1_600_000),
        counted_amount: vnd(1_600_000),
        ..report.clone()
    };
    let vietnamese = shift_report_document(
        &StoreProfile::default(),
        &style(0),
        &VIETNAMESE,
        &paid_out_only,
    );
    let drawn = lines_of(&vietnamese);
    assert!(
        drawn.contains(&"Chi ngoài bán hàng  VND 150,000"),
        "{drawn:?}"
    );
    assert!(
        !drawn
            .iter()
            .any(|line| line.starts_with("Thu ngoài bán hàng")),
        "nothing paid in prints no paid-in line: {drawn:?}"
    );
}

#[test]
fn a_vietnamese_store_prints_its_bill_and_its_drawer_in_vietnamese() {
    // The same documents, in the words a Vietnamese guest and cashier read: the heading, every
    // total's label and the sign-off change; the rows, the amounts and their order do not.
    let vnd = |minor| Money::new(CurrencyCode::VND, minor);
    let lines = [sold("Phở bò đặc biệt", 1_000, vnd(99_000), vnd(99_000))];
    let pre_bill = pre_bill_document(
        &StoreProfile::default(),
        &style(0),
        &VIETNAMESE,
        "3K9Q2Z",
        &lines,
        &totals_of(vnd(99_000)),
        None,
    );
    assert_eq!(
        lines_of(&pre_bill),
        vec![
            "PHIẾU TẠM TÍNH",
            "3K9Q2Z",
            "Phở bò đặc biệt",
            "  1 x VND 99,000  VND 99,000",
            "Tạm tính  VND 99,000",
            "VND 99,000",
            "Không phải hóa đơn thanh toán",
        ]
    );

    let report = ShiftReport {
        shift_id: pos_proto::ids::ShiftId::new(Ulid::from_u128(7)),
        opening_float: vnd(500_000),
        cash_collected: vnd(1_250_000),
        paid_in: vnd(0),
        paid_out: vnd(0),
        expected_amount: vnd(1_750_000),
        counted_amount: vnd(1_730_000),
        variance: vnd(-20_000),
        tax_components: Vec::new(),
    };
    let drawer = shift_report_document(&StoreProfile::default(), &style(0), &VIETNAMESE, &report);
    let drawn = lines_of(&drawer);
    assert_eq!(drawn.first(), Some(&"BÁO CÁO CA"));
    assert_eq!(drawn.last(), Some(&"Thiếu"));
    assert!(drawn.contains(&"Chênh lệch  VND -20,000"));
}
