// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! An X on paper: headed as closing nothing, in the paper's language.

use pos_core::business_date::StoreTimeZone;
use pos_proto::ShiftState;
use pos_proto::ids::{ShiftId, VendorId};
use pos_proto::money::{CurrencyCode, Money};
use pos_proto::store_profile::StoreProfile;
use pos_proto::text::DisplayName;
use pos_proto::time::{BusinessDate, Timestamp};
use pos_proto::ulid::Ulid;

use crate::app::{InAppTotal, ShiftX, StoreX, TenderTotals, XFigures};
use crate::paper_labels::{ENGLISH, VIETNAMESE};
use crate::printing::tests::{lines_of, style};
use crate::printing::{XPaper, x_document};

fn vnd(minor: i64) -> Money {
    Money::new(CurrencyCode::VND, minor)
}

fn figures(expected: Option<i64>) -> XFigures {
    XFigures {
        sales: vnd(330_000),
        bills: 2,
        tenders: TenderTotals {
            cash: vnd(165_000),
            card: vnd(165_000),
            qr: vnd(0),
            other: vnd(0),
            in_app: Vec::new(),
        },
        discounts: vnd(0),
        voided_bills: 0,
        voided_cash: vnd(0),
        paid_in: vnd(0),
        paid_out: vnd(85_000),
        dropped: vnd(0),
        expected_cash: expected.map(vnd),
        channels: Vec::new(),
    }
}

fn shift(expected: Option<i64>) -> ShiftX {
    ShiftX {
        shift_id: ShiftId::new(Ulid::from_u128(7)),
        till: None,
        till_name: Some(DisplayName::new("Bar till")),
        state: ShiftState::Open,
        business_date: BusinessDate::from_ymd(2026, 11, 3).expect("a date"),
        opened_time: Timestamp::from_milliseconds_since_epoch(1_793_667_600_000).expect("instant"),
        figures: figures(expected),
    }
}

#[test]
fn an_x_says_it_closes_nothing_and_shows_the_expectation_only_where_it_was_read() {
    let zone = StoreTimeZone::from_iana_name("Asia/Ho_Chi_Minh").expect("a real zone");
    let read = shift(Some(580_000));
    let paper = x_document(
        &StoreProfile::default(),
        &style(0),
        &ENGLISH,
        &zone,
        XPaper::Shift(&read),
    );
    let lines = lines_of(&paper);
    assert_eq!(lines[..2], ["X - NOT A CLOSE", "Bar till"]);
    assert!(lines.contains(&"Opened: 2026-11-03 08:00"), "{lines:?}");
    assert!(lines.contains(&"Sales  VND 330,000"), "{lines:?}");
    assert!(lines.contains(&"Bills: 2"), "{lines:?}");
    assert!(lines.contains(&"Card  VND 165,000"), "{lines:?}");
    assert!(
        !lines.iter().any(|line| line.starts_with("QR")),
        "a tender that took nothing"
    );
    assert!(lines.contains(&"Paid out  VND 85,000"), "{lines:?}");
    assert_eq!(lines.last(), Some(&"Expected in drawer  VND 580,000"));

    let blind = shift(None);
    let paper = x_document(
        &StoreProfile::default(),
        &style(0),
        &ENGLISH,
        &zone,
        XPaper::Shift(&blind),
    );
    assert!(
        !lines_of(&paper)
            .iter()
            .any(|line| line.starts_with("Expected"))
    );

    let day = StoreX {
        business_date: BusinessDate::from_ymd(2026, 11, 3).expect("a date"),
        all_closed: true,
        shifts: vec![read.clone(), read],
        totals: Some(figures(Some(1_160_000))),
    };
    let paper = x_document(
        &StoreProfile::default(),
        &style(0),
        &VIETNAMESE,
        &zone,
        XPaper::Store(&day),
    );
    let lines = lines_of(&paper);
    assert_eq!(
        lines[..4],
        [
            "X - KHÔNG PHẢI CHỐT CA",
            "TẤT CẢ QUẦY ĐÃ ĐÓNG CA",
            "TỔNG CỬA HÀNG",
            "Doanh thu  VND 330,000"
        ]
    );
}

/// ADR-0259 decision 5: money paid in a delivery app prints under the app's name, and "Other" is
/// what is left once each app's is taken out.
#[test]
fn an_x_names_each_delivery_apps_payments_apart_from_other() {
    let zone = StoreTimeZone::from_iana_name("Asia/Ho_Chi_Minh").expect("a real zone");
    let mut read = shift(Some(580_000));
    read.figures.tenders.other = vnd(238_000);
    read.figures.tenders.in_app = vec![InAppTotal {
        vendor_id: VendorId::new(Ulid::from_u128(0x0EE1)),
        name: Some(DisplayName::new("GrabFood")),
        amount: vnd(188_000),
    }];
    let paper = x_document(
        &StoreProfile::default(),
        &style(0),
        &VIETNAMESE,
        &zone,
        XPaper::Shift(&read),
    );
    let lines = lines_of(&paper);
    assert!(
        lines.contains(&"Trả qua GrabFood  VND 188,000"),
        "{lines:?}"
    );
    assert!(lines.contains(&"Khác  VND 50,000"), "{lines:?}");
}
