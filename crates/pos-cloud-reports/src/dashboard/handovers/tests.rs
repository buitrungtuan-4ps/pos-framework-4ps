// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! A drawer handed over counts its cash once, with the figures of the hands-on run that found it
//! counted twice (UX-3): till 1 handed over from A to B, till 2 one shift.

use std::collections::BTreeMap;

use pos_contract_tests::fixtures;
use pos_proto::BusinessDate;
use pos_proto::envelope::{EventEnvelope, EventTypeRef, RawPayload};
use pos_proto::events::EventType;
use pos_proto::ids::{ShiftId, StoreId};
use pos_proto::ulid::Ulid;
use serde_json::json;

use super::{KeptHandovers, fold_handovers};
use crate::dashboard::drops::fold_drops;
use crate::dashboard::floats::{KeptFloats, fold_closed_floats};
use crate::dashboard::rollup::fold_cash;
use crate::types::DailyCash;

const TILL_1: u128 = 0xB1;
const TILL_2: u128 = 0xB2;
const A: u128 = 0xA;
const B: u128 = 0xB;
const C: u128 = 0xC;

fn ulid(n: u128) -> String {
    Ulid::from_u128(n).to_string()
}

fn money(minor: i64) -> serde_json::Value {
    json!({ "currency_code": "VND", "amount_minor": minor })
}

fn event(
    kind: EventType,
    mut payload: serde_json::Value,
    till: u128,
    shift: u128,
) -> EventEnvelope<RawPayload> {
    payload["terminal_device_id"] = json!(ulid(till));
    let mut base = fixtures::activations(StoreId::new(Ulid::from_u128(1)), 1, 1).remove(0);
    base.business_date = BusinessDate::from_ymd(2026, 11, 3).expect("valid date");
    base.event_type = EventTypeRef::from_known(kind);
    base.shift_id = Some(ShiftId::new(Ulid::from_u128(shift)));
    base.data = RawPayload::encode(&payload).expect("encode payload");
    base
}

fn opened(till: u128, shift: u128, float: i64, from: Option<u128>) -> EventEnvelope<RawPayload> {
    let mut payload = json!({ "opened_shift_id": ulid(shift), "opening_float": money(float) });
    if let Some(from) = from {
        payload["handed_over_from_shift_id"] = json!(ulid(from));
    }
    event(EventType::CashShiftOpened, payload, till, shift)
}

fn moved(kind: EventType, till: u128, shift: u128, amount: i64) -> EventEnvelope<RawPayload> {
    let payload = json!({ "amount": money(amount), "reason_code_id": ulid(0xEE) });
    event(kind, payload, till, shift)
}

fn closed(till: u128, shift: u128, expected: i64, counted: i64) -> EventEnvelope<RawPayload> {
    let payload = json!({
        "closed_shift_id": ulid(shift), "expected_amount": money(expected),
        "counted_amount": money(counted), "variance": money(counted - expected),
    });
    event(EventType::CashShiftClosed, payload, till, shift)
}

/// The day of the run: A opens till 1 on 1,000,000, pays out 85,000, and closes 7,000 short at
/// 1,780,000; B takes it over on `b_float`, linked or not, and closes at `b_closes`; till 2 opens on
/// 500,000, pays in 200,000 and out 1,000,000, and closes 30,000 short.
fn the_day(linked: bool, b_float: i64, b_closes: i64) -> DailyCash {
    let events = [
        opened(TILL_1, A, 1_000_000, None),
        opened(TILL_2, C, 500_000, None),
        moved(EventType::CashDrawerPaidOut, TILL_1, A, 85_000),
        moved(EventType::CashDrawerPaidIn, TILL_2, C, 200_000),
        moved(EventType::CashDrawerPaidOut, TILL_2, C, 1_000_000),
        closed(TILL_1, A, 1_787_000, 1_780_000),
        opened(TILL_1, B, b_float, linked.then_some(A)),
        closed(TILL_1, B, b_closes, b_closes),
        closed(TILL_2, C, 1_073_000, 1_043_000),
    ];
    let (mut cash, mut floats, mut kept) = (
        BTreeMap::new(),
        KeptFloats::default(),
        KeptHandovers::default(),
    );
    for event in &events {
        fold_cash(&mut cash, event);
        fold_closed_floats(&mut floats, &mut cash, event);
        fold_drops(&mut cash, event);
        fold_handovers(&mut kept, &mut cash, event);
    }
    assert!(kept.carried.is_empty(), "every handed-over shift closed");
    cash.remove("2026-11-03").expect("the day")
}

#[test]
fn a_drawer_handed_over_counts_its_cash_once() {
    let day = the_day(true, 1_780_000, 2_205_000);
    let till = &day.by_drawer[&ulid(TILL_1)];
    assert_eq!(
        till.opening_float, 1_000_000,
        "the carried float is no new cash"
    );
    assert_eq!(till.closed_float, 1_000_000);
    assert_eq!(till.expected, 2_212_000, "A's sales and B's on A's float");
    assert_eq!(till.counted, 2_205_000, "what is in the drawer at the end");
    assert_eq!(
        till.variance, -7_000,
        "A's short, and nothing lost at the handover"
    );
    assert_eq!(till.handover_difference, 0);
    // The store expects what is really in its drawers, 3,248,000, plus what is short.
    assert_eq!(day.opening_float, 1_500_000);
    assert_eq!(day.expected, 3_285_000);
    assert_eq!(day.counted, 3_248_000);
    assert_eq!(day.variance, -37_000);
    assert_eq!(day.counted - day.expected, day.variance, "it still adds up");
    assert!(day.by_shift[&ulid(B)].handed_over);
}

#[test]
fn a_handover_difference_is_the_drawers_and_the_days() {
    // B finds 10,000 less than A counted out, and balances against what B found.
    let day = the_day(true, 1_770_000, 2_195_000);
    let till = &day.by_drawer[&ulid(TILL_1)];
    assert_eq!(till.handover_difference, -10_000);
    assert_eq!(day.handover_difference, -10_000);
    assert_eq!(till.expected, 2_212_000);
    assert_eq!(till.counted, 2_195_000);
    assert_eq!(
        till.variance, -17_000,
        "A's 7,000 and the handover's 10,000"
    );
}

#[test]
fn shifts_not_linked_keep_their_own_rows() {
    // Older data names no handover: the drawer sums as before, and each shift reads on its own.
    let day = the_day(false, 1_780_000, 2_205_000);
    let till = &day.by_drawer[&ulid(TILL_1)];
    assert_eq!((till.opening_float, till.expected), (2_780_000, 3_992_000));
    let a = &day.by_shift[&ulid(A)];
    assert_eq!(
        (
            a.opening_float,
            a.paid_out,
            a.expected,
            a.counted,
            a.variance,
            a.closed
        ),
        (1_000_000, 85_000, 1_787_000, 1_780_000, -7_000, true)
    );
    let b = &day.by_shift[&ulid(B)];
    assert_eq!(
        (b.opening_float, b.expected, b.handed_over),
        (1_780_000, 2_205_000, false)
    );
    let c = &day.by_shift[&ulid(C)];
    assert_eq!(
        (c.paid_in, c.paid_out, c.variance),
        (200_000, 1_000_000, -30_000)
    );
    assert_eq!(day.handover_difference, 0);
}
