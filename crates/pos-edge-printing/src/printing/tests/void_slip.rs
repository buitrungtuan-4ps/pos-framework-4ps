// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The void slip and the VOID copy (ADR-0184 decisions 4 to 6, and its amendment of 2026-11-02).

use pos_proto::money::{CurrencyCode, Money};
use pos_proto::store_profile::StoreProfile;

use crate::paper_labels::{ENGLISH, VIETNAMESE};
use crate::printing::tests::{lines_of, style, totals_of};
use crate::printing::{VoidSlip, receipt_copy_document, void_slip_document};

fn vnd(minor: i64) -> Money {
    Money::new(CurrencyCode::VND, minor)
}

fn slip(cash: i64, other: i64) -> VoidSlip {
    VoidSlip {
        receipt_number: 42,
        total_due: vnd(cash + other),
        cash_returned: vnd(cash),
        other_reversed: vnd(other),
        reason: Some("Keyed in error".to_owned()),
        till: Some("Bar".to_owned()),
        voided_at: "2026-11-02 19:05".to_owned(),
    }
}

/// The receipt's number, the amount, the reason, the till, the time and the cash handed back, and
/// no person.
#[test]
fn a_cash_void_slip_names_the_receipt_the_amount_and_the_reason_and_nobody() {
    let document = void_slip_document(
        &StoreProfile::default(),
        &style(0),
        &ENGLISH,
        &slip(165_000, 0),
    );
    assert_eq!(
        lines_of(&document),
        [
            "BILL VOIDED",
            "#42",
            "Amount  VND 165,000",
            "Reason: Keyed in error",
            "Till: Bar",
            "Time: 2026-11-02 19:05",
            "Cash returned  VND 165,000",
            "VOID",
        ]
    );
}

/// A card, or any tender but cash, is reversed on the terminal, which the till does not drive, so
/// the slip says how much and reminds the cashier.
#[test]
fn a_card_void_slip_reminds_the_cashier_to_reverse_it_on_the_terminal() {
    let document = void_slip_document(
        &StoreProfile::default(),
        &style(0),
        &ENGLISH,
        &VoidSlip {
            till: None,
            ..slip(65_000, 100_000)
        },
    );
    let lines = lines_of(&document);
    assert_eq!(
        lines[lines.len() - 4..],
        [
            "Cash returned  VND 65,000",
            "Card and other  VND 100,000",
            "Reverse it on the card terminal",
            "VOID",
        ]
    );
    assert!(
        !lines.iter().any(|line| line.starts_with("Till")),
        "a device that is no till prints no till line"
    );
}

#[test]
fn a_vietnamese_store_prints_the_slip_in_vietnamese() {
    let document = void_slip_document(
        &StoreProfile::default(),
        &style(0),
        &VIETNAMESE,
        &slip(0, 165_000),
    );
    let lines = lines_of(&document);
    assert_eq!(lines[0], "HÓA ĐƠN ĐÃ HỦY");
    assert!(lines.contains(&"Lý do: Keyed in error"));
    assert!(lines.contains(&"Hoàn tiền trên máy quẹt thẻ"));
    assert!(
        !lines.iter().any(|line| line.starts_with("Tiền mặt")),
        "no cash, no cash line"
    );
}

/// A copy of a voided bill's receipt is marked VOID above COPY; any other copy is not.
#[test]
fn a_copy_of_a_voided_bills_receipt_is_marked_void() {
    let copy = |voided| {
        receipt_copy_document(
            &StoreProfile::default(),
            &style(0),
            &ENGLISH,
            (42, 1, voided),
            "19:05",
            &[],
            &totals_of(vnd(165_000)),
            None,
            None,
        )
    };
    assert_eq!(lines_of(&copy(true))[..3], ["#42", "VOID", "COPY"]);
    assert_eq!(lines_of(&copy(false))[..2], ["#42", "COPY"]);
}
