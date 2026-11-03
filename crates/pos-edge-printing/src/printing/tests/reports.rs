// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The shift reports on paper, and a Vietnamese store's bill and drawer in Vietnamese.

use pos_core::cash_count::CountBreakdown;
use pos_ports::printer::PrintBlock;
use pos_proto::money::{CurrencyCode, Money};
use pos_proto::store_profile::StoreProfile;
use pos_proto::text::DisplayName;
use pos_proto::ulid::Ulid;

use crate::app::{CombinedReport, DrawerFigures, ShiftHeader, ShiftReport};
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
        dropped: vnd(0),
        expected_amount: vnd(1_750_000),
        counted_amount: vnd(counted),
        count_breakdown: CountBreakdown::default(),
        variance: vnd(counted - 1_750_000),
        voided_bills: 0,
        voided_cash: vnd(0),
        tax_components: Vec::new(),
        header: ShiftHeader::default(),
    };

    let short = shift_report_document(
        &StoreProfile::default(),
        &style(0),
        &ENGLISH,
        &report(1_730_000),
    );
    let lines = lines_of(&short);
    assert_eq!(lines[0], "SHIFT REPORT");
    assert_eq!(lines[2], "Bills: 0");
    assert_eq!(
        lines[3..],
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
        dropped: vnd(0),
        expected_amount: vnd(float + 100_000),
        counted_amount: vnd(counted),
        count_breakdown: CountBreakdown::default(),
        variance: vnd(counted - float - 100_000),
        voided_bills: 0,
        voided_cash: vnd(0),
        tax_components: Vec::new(),
        header: ShiftHeader::default(),
    };
    let bar = drawer(0xBA, 500_000, 590_000);
    let counter = drawer(0xC0, 300_000, 400_000);
    let combined = CombinedReport {
        totals: DrawerFigures {
            opening_float: vnd(800_000),
            cash_collected: vnd(200_000),
            paid_in: vnd(0),
            paid_out: vnd(0),
            dropped: vnd(0),
            expected_amount: vnd(1_000_000),
            counted_amount: vnd(990_000),
            variance: vnd(-10_000),
            voided_cash: vnd(0),
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
        lines[11..15],
        figures(
            "Opening float  VND 500,000",
            "Expected in drawer  VND 600,000",
            "Counted  VND 590,000"
        )
    );
    assert_eq!(lines[17], "Counter");
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
        dropped: vnd(0),
        expected_amount: vnd(1_800_000),
        counted_amount: vnd(1_800_000),
        count_breakdown: CountBreakdown::default(),
        variance: vnd(0),
        voided_bills: 0,
        voided_cash: vnd(0),
        tax_components: Vec::new(),
        header: ShiftHeader::default(),
    };
    let english = shift_report_document(&StoreProfile::default(), &style(0), &ENGLISH, &report);
    assert_eq!(
        lines_of(&english)[3..],
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
        count_breakdown: CountBreakdown::default(),
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
        dropped: vnd(0),
        expected_amount: vnd(1_750_000),
        counted_amount: vnd(1_730_000),
        count_breakdown: CountBreakdown::default(),
        variance: vnd(-20_000),
        voided_bills: 0,
        voided_cash: vnd(0),
        tax_components: Vec::new(),
        header: ShiftHeader::default(),
    };
    let drawer = shift_report_document(&StoreProfile::default(), &style(0), &VIETNAMESE, &report);
    let drawn = lines_of(&drawer);
    assert_eq!(drawn.first(), Some(&"BÁO CÁO CA"));
    assert_eq!(drawn.last(), Some(&"Thiếu"));
    assert!(drawn.contains(&"Chênh lệch  VND -20,000"));
}

/// A drawer that handed cash back for bills voided after payment prints it among the terms its
/// expectation sums, so the paper still adds up (ADR-0184); one that handed none back prints no line.
#[test]
fn a_shift_report_prints_the_cash_voids_handed_back() {
    let vnd = |minor| Money::new(CurrencyCode::VND, minor);
    let report = ShiftReport {
        shift_id: pos_proto::ids::ShiftId::new(Ulid::from_u128(7)),
        opening_float: vnd(500_000),
        cash_collected: vnd(1_250_000),
        paid_in: vnd(0),
        paid_out: vnd(0),
        dropped: vnd(0),
        expected_amount: vnd(1_635_000),
        counted_amount: vnd(1_635_000),
        count_breakdown: CountBreakdown::default(),
        variance: vnd(0),
        voided_bills: 1,
        voided_cash: vnd(115_000),
        tax_components: Vec::new(),
        header: ShiftHeader::default(),
    };
    let english = shift_report_document(&StoreProfile::default(), &style(0), &ENGLISH, &report);
    assert_eq!(
        lines_of(&english)[3..7],
        [
            "Opening float  VND 500,000",
            "Cash taken  VND 1,250,000",
            "Voided after payment  VND 115,000",
            "Expected in drawer  VND 1,635,000",
        ]
    );
    let none = ShiftReport {
        voided_bills: 0,
        voided_cash: vnd(0),
        ..report
    };
    let drawn = shift_report_document(&StoreProfile::default(), &style(0), &ENGLISH, &none);
    assert!(
        !lines_of(&drawn)
            .iter()
            .any(|line| line.starts_with("Voided")),
        "no void, no line"
    );
}

#[test]
fn a_count_made_note_by_note_lists_its_notes_under_the_figures() {
    // ADR-0186: the rows a supervisor can check pile by pile, and nothing for a typed count.
    use pos_proto::events::DenominationCount;
    let vnd = |minor| Money::new(CurrencyCode::VND, minor);
    let row = |denomination, quantity| DenominationCount {
        denomination: vnd(denomination),
        quantity,
    };
    let report = ShiftReport {
        shift_id: pos_proto::ids::ShiftId::new(Ulid::from_u128(7)),
        opening_float: vnd(500_000),
        cash_collected: vnd(703_000),
        paid_in: vnd(0),
        paid_out: vnd(0),
        dropped: vnd(0),
        expected_amount: vnd(1_203_000),
        counted_amount: vnd(1_203_000),
        count_breakdown: CountBreakdown {
            denomination_counts: vec![row(500_000, 2), row(100_000, 2)],
            uncounted_amount: Some(vnd(3_000)),
        },
        variance: vnd(0),
        voided_bills: 0,
        voided_cash: vnd(0),
        tax_components: Vec::new(),
        header: ShiftHeader::default(),
    };
    let document = shift_report_document(&StoreProfile::default(), &style(0), &ENGLISH, &report);
    let lines = lines_of(&document);
    let at = lines
        .iter()
        .position(|line| *line == "COUNT BY NOTE")
        .expect("the rows have a heading");
    assert_eq!(
        lines[at + 1..at + 4],
        [
            "VND 500,000 x 2  VND 1,000,000",
            "VND 100,000 x 2  VND 200,000",
            "Other / uncounted  VND 3,000",
        ]
    );
}

#[test]
fn a_drop_to_the_safe_prints_apart_from_the_paid_out() {
    // ADR-0186 decision 5: it lowers the expectation as a paid out does, under its own words, so
    // nobody reads it as an expense.
    let vnd = |minor| Money::new(CurrencyCode::VND, minor);
    let report = ShiftReport {
        shift_id: pos_proto::ids::ShiftId::new(Ulid::from_u128(7)),
        opening_float: vnd(500_000),
        cash_collected: vnd(1_250_000),
        paid_in: vnd(0),
        paid_out: vnd(150_000),
        dropped: vnd(1_000_000),
        expected_amount: vnd(600_000),
        counted_amount: vnd(600_000),
        count_breakdown: CountBreakdown::default(),
        variance: vnd(0),
        voided_bills: 0,
        voided_cash: vnd(0),
        tax_components: Vec::new(),
        header: ShiftHeader::default(),
    };
    let english = shift_report_document(&StoreProfile::default(), &style(0), &ENGLISH, &report);
    assert_eq!(
        lines_of(&english)[5..8],
        [
            "Paid out  VND 150,000",
            "Dropped to safe  VND 1,000,000",
            "Expected in drawer  VND 600,000",
        ]
    );
    let vietnamese =
        shift_report_document(&StoreProfile::default(), &style(0), &VIETNAMESE, &report);
    assert!(lines_of(&vietnamese).contains(&"Nộp két  VND 1,000,000"));
}

#[test]
fn a_shift_report_says_which_till_when_how_many_bills_by_which_tender_and_why() {
    // UX-4: the paper names the till and its times, the bills and the tender mix, and the
    // close's reason, and no person.
    use pos_proto::PaymentMethod;
    use pos_proto::reason_codes::{PublishedReasonCode, ReasonAction, ReasonCode};
    let vnd = |minor| Money::new(CurrencyCode::VND, minor);
    let reason = PublishedReasonCode::new(
        pos_proto::ids::ReasonCodeId::new(Ulid::from_u128(9)),
        ReasonCode::new("COUNT_DIFFERENCE"),
        DisplayName::new("Counting difference"),
        vec![ReasonAction::CashVariance],
    );
    let report = ShiftReport {
        shift_id: pos_proto::ids::ShiftId::new(Ulid::from_u128(7)),
        opening_float: vnd(500_000),
        cash_collected: vnd(300_000),
        paid_in: vnd(0),
        paid_out: vnd(0),
        dropped: vnd(0),
        expected_amount: vnd(800_000),
        counted_amount: vnd(770_000),
        count_breakdown: CountBreakdown::default(),
        variance: vnd(-30_000),
        voided_bills: 0,
        voided_cash: vnd(0),
        tax_components: Vec::new(),
        header: ShiftHeader {
            till_name: Some(DisplayName::new("Bar")),
            opened_at: Some("2026-11-03 08:00".to_owned()),
            closed_at: Some("2026-11-03 16:05".to_owned()),
            bills: 3,
            tenders: vec![
                (PaymentMethod::Cash, vnd(300_000)),
                (PaymentMethod::Card, vnd(450_000)),
                (PaymentMethod::Qr, vnd(120_000)),
            ],
            variance_reason: Some(reason),
        },
    };
    let document = shift_report_document(&StoreProfile::default(), &style(0), &ENGLISH, &report);
    let lines = lines_of(&document);
    assert_eq!(lines[..2], ["SHIFT REPORT", "Bar"]);
    assert_eq!(
        lines[3..9],
        [
            "Opened: 2026-11-03 08:00",
            "Closed: 2026-11-03 16:05",
            "Bills: 3",
            "Cash  VND 300,000",
            "Card  VND 450,000",
            "QR  VND 120,000",
        ]
    );
    assert_eq!(lines.last(), Some(&"Reason: Counting difference"));
}
