// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! A drawer's closed float is the float of its shifts that closed, beside the figures their closes
//! brought, while `opening_float` still adds every shift opened.

use std::collections::BTreeMap;

use pos_contract_tests::fixtures;
use pos_proto::BusinessDate;
use pos_proto::envelope::{EventEnvelope, EventTypeRef, RawPayload};
use pos_proto::events::EventType;
use pos_proto::ids::StoreId;
use pos_proto::ulid::Ulid;
use serde_json::json;

use super::{KeptFloats, fold_closed_floats};
use crate::dashboard::projection::StoredRollups;
use crate::dashboard::rollup::fold_cash;
use crate::types::{DailyCash, STORE_DRAWER};

const TILL: u128 = 0xBA;

fn ulid(n: u128) -> String {
    Ulid::from_u128(n).to_string()
}

fn money(minor: i64) -> serde_json::Value {
    json!({ "currency_code": "VND", "amount_minor": minor })
}

fn event(
    date: &str,
    event_type: EventType,
    mut payload: serde_json::Value,
    till: Option<u128>,
) -> EventEnvelope<RawPayload> {
    if let Some(till) = till {
        payload["terminal_device_id"] = json!(ulid(till));
    }
    let mut base = fixtures::activations(StoreId::new(Ulid::from_u128(1)), 1, 1).remove(0);
    base.business_date = BusinessDate::from_ymd(
        date[0..4].parse().expect("year"),
        date[5..7].parse().expect("month"),
        date[8..10].parse().expect("day"),
    )
    .expect("valid date");
    base.event_type = EventTypeRef::from_known(event_type);
    base.data = RawPayload::encode(&payload).expect("encode payload");
    base
}

fn opened(date: &str, shift: u128, float: i64, till: Option<u128>) -> EventEnvelope<RawPayload> {
    let payload = json!({ "opened_shift_id": ulid(shift), "opening_float": money(float) });
    event(date, EventType::CashShiftOpened, payload, till)
}

fn closed(date: &str, shift: u128, expected: i64, till: Option<u128>) -> EventEnvelope<RawPayload> {
    let payload = json!({
        "closed_shift_id": ulid(shift), "expected_amount": money(expected),
        "counted_amount": money(expected), "variance": money(0),
    });
    event(date, EventType::CashShiftClosed, payload, till)
}

/// The projector's cash folds, in its order, over `events`.
fn folded(events: &[EventEnvelope<RawPayload>]) -> (BTreeMap<String, DailyCash>, KeptFloats) {
    let (mut cash, mut kept) = (BTreeMap::new(), KeptFloats::default());
    for event in events {
        fold_cash(&mut cash, event);
        fold_closed_floats(&mut kept, &mut cash, event);
    }
    (cash, kept)
}

const DAY: &str = "2026-03-15";

/// The X report of a till whose first shift closed and whose second is open: the closed float is
/// the first shift's, beside the first shift's expected amount, while the floats opened add both.
#[test]
fn a_drawer_with_its_second_shift_open_sets_only_the_closed_shifts_float_beside_its_close() {
    let events = [
        opened(DAY, 1, 500_000, Some(TILL)),
        closed(DAY, 1, 1_570_000, Some(TILL)),
        opened(DAY, 2, 300_000, Some(TILL)),
    ];
    let (cash, kept) = folded(&events);
    let day = cash.get(DAY).expect("the day");
    let drawer = day.by_drawer.get(&ulid(TILL)).expect("the till's drawer");

    assert_eq!(
        drawer.closed_float, 500_000,
        "the first shift's float alone"
    );
    assert_eq!(
        drawer.opening_float, 800_000,
        "opening_float keeps its meaning"
    );
    assert_eq!(drawer.expected, 1_570_000);
    assert_eq!((day.closed_float, day.opening_float), (500_000, 800_000));
    assert_eq!(
        kept.shifts.into_iter().collect::<Vec<_>>(),
        [(ulid(2), 300_000)],
        "the closed shift is let go, the open one is kept"
    );
}

/// A shift opened one day and closed the next counts its float as closed on the day of its close,
/// the day its expected amount is on, under the store's drawer when it names no till.
#[test]
fn a_shift_closed_the_next_day_counts_its_float_on_the_day_of_its_close() {
    let events = [
        opened("2026-03-14", 1, 400_000, None),
        closed(DAY, 1, 450_000, None),
    ];
    let (cash, kept) = folded(&events);
    let before = cash.get("2026-03-14").expect("the day it opened");
    let after = cash.get(DAY).expect("the day it closed");

    assert_eq!((before.opening_float, before.closed_float), (400_000, 0));
    assert_eq!((after.opening_float, after.closed_float), (0, 400_000));
    let drawer = after
        .by_drawer
        .get(STORE_DRAWER)
        .expect("the store's drawer");
    assert_eq!((drawer.closed_float, drawer.expected), (400_000, 450_000));
    assert!(kept.shifts.is_empty());
}

/// A shift opened before the rollup kept floats adds nothing at its close, and a rollup stored
/// before then loads with none kept.
#[test]
fn a_shift_the_rollup_never_saw_open_adds_no_closed_float() {
    let (cash, _) = folded(&[closed(DAY, 9, 100_000, None)]);
    let day = cash.get(DAY).expect("the day");
    assert_eq!((day.closed_float, day.expected), (0, 100_000));

    let stored: StoredRollups =
        serde_json::from_value(json!({ "cursor": null, "days": {} })).expect("an older blob");
    assert!(stored.floats.shifts.is_empty());
}
