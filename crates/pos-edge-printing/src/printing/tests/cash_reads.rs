// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The cash papers that close nothing: the handover slip.

use pos_core::cash_count::CountBreakdown;
use pos_proto::events::DenominationCount;
use pos_proto::ids::ShiftId;
use pos_proto::money::{CurrencyCode, Money};
use pos_proto::store_profile::StoreProfile;
use pos_proto::text::DisplayName;
use pos_proto::ulid::Ulid;

use crate::app::HandoverSlip;
use crate::paper_labels::{ENGLISH, VIETNAMESE};
use crate::printing::handover_slip_document;
use crate::printing::tests::{lines_of, style};

fn vnd(minor: i64) -> Money {
    Money::new(CurrencyCode::VND, minor)
}

fn slip(incoming: i64) -> HandoverSlip {
    HandoverSlip {
        till_name: Some(DisplayName::new("Bar till")),
        handed_over_at: Some("2026-11-03 14:00".to_owned()),
        outgoing_shift_id: ShiftId::new(Ulid::from_u128(7)),
        outgoing_count: vnd(1_780_000),
        outgoing_breakdown: CountBreakdown {
            denomination_counts: vec![DenominationCount {
                denomination: vnd(500_000),
                quantity: 3,
            }],
            uncounted_amount: Some(vnd(280_000)),
        },
        incoming_shift_id: ShiftId::new(Ulid::from_u128(8)),
        incoming_count: vnd(incoming),
        incoming_breakdown: CountBreakdown::default(),
        difference: vnd(incoming - 1_780_000),
    }
}

#[test]
fn a_handover_slip_shows_both_counts_and_the_difference_and_names_no_person() {
    // ADR-0187 decision 3: the till, the time, each count with its notes, and the difference with
    // its verdict in words.
    let short = handover_slip_document(
        &StoreProfile::default(),
        &style(0),
        &ENGLISH,
        &slip(1_770_000),
    );
    let lines = lines_of(&short);
    assert_eq!(
        lines[..3],
        ["SHIFT HANDOVER", "Bar till", "Time: 2026-11-03 14:00"]
    );
    assert!(lines.contains(&"Counted out  VND 1,780,000"), "{lines:?}");
    assert!(
        lines.contains(&"VND 500,000 x 3  VND 1,500,000"),
        "{lines:?}"
    );
    assert!(
        lines.contains(&"Other / uncounted  VND 280,000"),
        "{lines:?}"
    );
    assert!(lines.contains(&"Counted in  VND 1,770,000"), "{lines:?}");
    assert_eq!(
        lines[lines.len() - 2..],
        ["Difference  VND -10,000", "Short"]
    );

    let agreed = handover_slip_document(
        &StoreProfile::default(),
        &style(0),
        &VIETNAMESE,
        &slip(1_780_000),
    );
    let lines = lines_of(&agreed);
    assert_eq!(lines[0], "GIAO CA");
    assert_eq!(lines.last(), Some(&"Khớp"));
}
