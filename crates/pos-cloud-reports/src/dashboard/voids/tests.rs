// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! A bill voided after payment leaves its day's sales, tax, fees and components, counts as voided
//! after payment, and its cash shows against the drawer of the void's shift (ADR-0184).

use std::collections::BTreeMap;

use pos_contract_tests::fixtures;
use pos_proto::BusinessDate;
use pos_proto::envelope::{EventEnvelope, EventTypeRef, RawPayload};
use pos_proto::events::EventType;
use pos_proto::ids::{ShiftId, StoreId};
use pos_proto::ulid::Ulid;
use serde_json::json;

use super::{KeptBills, fold_voids};
use crate::dashboard::rollup::{fold_cash, fold_revenue};
use crate::types::{DailyCash, DailyRevenue, STORE_DRAWER};

fn ulid(n: u128) -> String {
    Ulid::from_u128(n).to_string()
}

fn money(minor: i64) -> serde_json::Value {
    json!({ "currency_code": "VND", "amount_minor": minor })
}

fn event(
    date: &str,
    event_type: EventType,
    payload: &serde_json::Value,
    shift: Option<u128>,
) -> EventEnvelope<RawPayload> {
    let mut base = fixtures::activations(StoreId::new(Ulid::from_u128(1)), 1, 1).remove(0);
    base.business_date = BusinessDate::from_ymd(
        date[0..4].parse().expect("year"),
        date[5..7].parse().expect("month"),
        date[8..10].parse().expect("day"),
    )
    .expect("valid date");
    base.event_type = EventTypeRef::from_known(event_type);
    base.data = RawPayload::encode(payload).expect("encode payload");
    base.shift_id = shift.map(|n| ShiftId::new(Ulid::from_u128(n)));
    base
}

/// A bill paid by `cash` and `card`: 100,000 sold, a 5,000 fee taxed 500, 8,000 tax with one
/// component, so it owes 113,000 (cash plus card).
fn paid(date: &str, bill: u128, (cash, card): (i64, i64)) -> Vec<EventEnvelope<RawPayload>> {
    let mut events = Vec::new();
    for (method, amount) in [("PAYMENT_METHOD_CASH", cash), ("PAYMENT_METHOD_CARD", card)] {
        if amount != 0 {
            events.push(event(
                date,
                EventType::BillingPaymentCaptured,
                &json!({
                    "bill_id": ulid(bill), "payment_id": ulid(bill * 10 + 1), "method": method,
                    "outcome": "PAYMENT_OUTCOME_CAPTURED", "tendered": money(amount),
                    "applied_to_bill": money(amount), "change_given": money(0),
                    "tip_amount": money(0),
                }),
                Some(7),
            ));
        }
    }
    events.push(event(
        date,
        EventType::BillingBillSettled,
        &json!({
            "bill_id": ulid(bill), "receipt_number": 1u64,
            "subtotal": money(100_000), "reduction_total": money(0),
            "service_charge": money(5_000), "tax_total": money(8_000),
            "rounding_adjustment": money(0), "total_due": money(113_000),
            "fee_lines": [{
                "fee_id": ulid(9), "code": "SVC", "display_name": "Service",
                "amount": money(5_000), "tax": money(500),
            }],
            "tax_lines": [{
                "tax_class_id": ulid(3), "taxable_base": money(105_000),
                "rate_basis_points": 800, "tax": money(8_000),
                "components": [{ "name": "VAT", "rate_basis_points": 800, "tax": money(8_000) }],
            }],
        }),
        Some(7),
    ));
    events
}

fn voided(date: &str, bill: u128, shift: Option<u128>) -> EventEnvelope<RawPayload> {
    event(
        date,
        EventType::BillingBillVoided,
        &json!({ "bill_id": ulid(bill), "reason_code_id": ulid(99) }),
        shift,
    )
}

fn shift_opened(date: &str, shift: u128, till: Option<u128>) -> EventEnvelope<RawPayload> {
    let mut payload = json!({ "opened_shift_id": ulid(shift), "opening_float": money(0) });
    if let Some(till) = till {
        payload["terminal_device_id"] = json!(ulid(till));
    }
    event(date, EventType::CashShiftOpened, &payload, Some(shift))
}

/// The projector's folds, in its order, over `events`.
fn folded(
    events: &[EventEnvelope<RawPayload>],
) -> (
    BTreeMap<String, DailyRevenue>,
    BTreeMap<String, DailyCash>,
    KeptBills,
) {
    let (mut revenue, mut cash, mut kept) =
        (BTreeMap::new(), BTreeMap::new(), KeptBills::default());
    for event in events {
        fold_revenue(&mut revenue, event);
        fold_cash(&mut cash, event);
        fold_voids(&mut kept, &mut revenue, &mut cash, event);
    }
    (revenue, cash, kept)
}

const DAY: &str = "2026-11-02";

/// Two bills paid, one voided: the day's every figure is the other bill's alone, and the voided one
/// is counted with what it had taken.
#[test]
fn a_voided_bill_leaves_the_days_sales_tax_and_fees_and_is_counted_apart() {
    let mut events = paid(DAY, 1, (113_000, 0));
    events.extend(paid(DAY, 2, (0, 113_000)));
    let (only_the_second, _, _) = folded(&events[3..]);
    events.push(voided(DAY, 1, Some(7)));
    let (revenue, _, kept) = folded(&events);

    let day = revenue.get(DAY).expect("the day");
    let kept_day = only_the_second.get(DAY).expect("the day");
    assert_eq!(
        (day.bills, day.gross, day.service_charge, day.tax, day.net),
        (
            kept_day.bills,
            kept_day.gross,
            kept_day.service_charge,
            kept_day.tax,
            kept_day.net
        ),
        "the sales, tax and totals of the bill that stayed paid"
    );
    assert_eq!(day.by_fee, kept_day.by_fee);
    assert_eq!(day.by_tax_component, kept_day.by_tax_component);
    assert_eq!(day.voided_after_payment.bills, 1);
    assert_eq!(day.voided_after_payment.amount, 113_000);
    assert!(
        !kept.days.get(DAY).expect("kept").contains_key(&ulid(1)),
        "a voided bill is not kept to be voided twice"
    );
}

/// The cash of a voided bill shows against the drawer whose shift the void was stamped with, and
/// the store's figure is the drawers' sum; a card void hands back no cash.
#[test]
fn a_cash_void_shows_against_the_voiding_tills_drawer() {
    let mut events = vec![shift_opened(DAY, 7, Some(500)), shift_opened(DAY, 8, None)];
    events.extend(paid(DAY, 1, (100_000, 13_000)));
    events.extend(paid(DAY, 2, (0, 113_000)));
    events.push(voided(DAY, 1, Some(7)));
    events.push(voided(DAY, 2, Some(8)));
    let (_, cash, _) = folded(&events);

    let day = cash.get(DAY).expect("the day");
    assert_eq!(day.voided_cash, 100_000, "only the cash part, not the card");
    assert_eq!(
        day.by_drawer
            .get(&ulid(500))
            .map(|drawer| drawer.voided_cash),
        Some(100_000)
    );
    assert_eq!(
        day.by_drawer
            .get(STORE_DRAWER)
            .map(|drawer| drawer.voided_cash),
        Some(0)
    );
}

/// A void of a bill nobody paid, a second void of the same bill, and one with no shift change
/// nothing beyond what they should.
#[test]
fn only_a_paid_bill_is_taken_back_and_only_once() {
    let mut events = paid(DAY, 1, (113_000, 0));
    events.push(voided(DAY, 3, None));
    let (before, _, _) = folded(&events);
    events.push(voided(DAY, 1, None));
    events.push(voided(DAY, 1, None));
    let (revenue, cash, _) = folded(&events);

    let day = revenue.get(DAY).expect("the day");
    assert_eq!(before.get(DAY).map(|day| day.bills), Some(1));
    assert_eq!((day.bills, day.net, day.tax), (0, 0, 0));
    assert_eq!(day.voided_after_payment.bills, 1, "voided once");
    assert!(
        day.by_fee.is_empty(),
        "the only bill that charged it is gone"
    );
    // No shift on the void: the store's one drawer.
    assert_eq!(
        cash.get(DAY)
            .and_then(|day| day.by_drawer.get(STORE_DRAWER))
            .map(|drawer| drawer.voided_cash),
        Some(113_000)
    );
}

/// A bill voided on a later business date leaves the day it was sold on, which counts the void;
/// the cash shows on the day it left the drawer.
#[test]
fn a_void_on_a_later_day_restates_the_bills_own_day() {
    let mut events = paid("2026-11-01", 1, (113_000, 0));
    events.push(voided(DAY, 1, None));
    let (revenue, cash, _) = folded(&events);
    let sold_on = revenue.get("2026-11-01").expect("the day it sold");
    assert_eq!((sold_on.bills, sold_on.voided_after_payment.bills), (0, 1));
    assert_eq!(cash.get(DAY).map(|day| day.voided_cash), Some(113_000));
}

/// Nothing voided: the day serialises as it did before the field existed.
#[test]
fn a_day_with_no_void_writes_no_void_fields() {
    let (revenue, cash, _) = folded(&paid(DAY, 1, (113_000, 0)));
    let revenue = serde_json::to_value(revenue.get(DAY).expect("day")).expect("json");
    assert!(revenue.get("voided_after_payment").is_none());
    let mut opened = vec![shift_opened(DAY, 7, None)];
    opened.extend(paid(DAY, 2, (1, 0)));
    let (_, cash_only, _) = folded(&opened);
    let cash_json = serde_json::to_value(cash_only.get(DAY).expect("day")).expect("json");
    assert!(cash_json.get("voided_cash").is_none());
    assert!(
        !cash.contains_key(DAY),
        "a bill paid with no shift event makes no cash day"
    );
}
