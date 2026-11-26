// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

use pos_contract_tests::fixtures;
use pos_proto::envelope::{EventEnvelope, EventTypeRef, RawPayload};
use pos_proto::events::EventType;
use pos_proto::ids::StoreId;
use pos_proto::ulid::Ulid;
use serde_json::{Value, json};

use super::{KeptVendors, MAX_WAITING, VendorDay, fold_vendors};

const VENDOR: &str = "01J0000000000000000000CN01";

fn id(n: u128) -> String {
    Ulid::from_u128(n).to_string()
}

fn event(kind: EventType, data: &Value) -> EventEnvelope<RawPayload> {
    let mut base = fixtures::activations(StoreId::new(Ulid::from_u128(1)), 1, 1).remove(0);
    base.event_type = EventTypeRef::from_known(kind);
    base.data = RawPayload::encode(data).expect("encode");
    base
}

fn opened(order: u128, vendor: Option<&str>) -> EventEnvelope<RawPayload> {
    let mut data = json!({ "order_id": id(order), "channel": "SALES_CHANNEL_DELIVERY" });
    if let Some(vendor) = vendor {
        data["vendor_id"] = json!(vendor);
    }
    event(EventType::SalesOrderOpened, &data)
}

fn bill(bill: u128, order: u128) -> EventEnvelope<RawPayload> {
    event(
        EventType::BillingBillOpened,
        &json!({ "bill_id": id(bill), "order_id": id(order), "order_line_ids": [] }),
    )
}

fn settled(bill: u128, total: i64) -> EventEnvelope<RawPayload> {
    let money = |minor: i64| json!({ "currency_code": "VND", "amount_minor": minor });
    event(
        EventType::BillingBillSettled,
        &json!({
            "bill_id": id(bill), "receipt_number": 1, "subtotal": money(total),
            "reduction_total": money(0), "service_charge": money(0), "tax_total": money(0),
            "rounding_adjustment": money(0), "total_due": money(total),
        }),
    )
}

/// ADR-0248 decision 3: a vendor's settled orders add up per day, gross; another order adds
/// nothing, and an order closed unpaid is forgotten.
#[test]
fn a_vendor_s_settled_orders_add_up_by_day_and_nothing_else_does() {
    let mut kept = KeptVendors::default();
    for event in [
        opened(1, Some(VENDOR)),
        bill(11, 1),
        settled(11, 132_000),
        opened(2, Some(VENDOR)),
        bill(12, 2),
        settled(12, 110_000),
        opened(3, None),
        bill(13, 3),
        settled(13, 99_000),
        opened(4, Some(VENDOR)),
        event(EventType::SalesOrderClosed, &json!({ "order_id": id(4) })),
    ] {
        fold_vendors(&mut kept, &event);
    }
    let day = kept.days.values().next().expect("the day");
    assert_eq!(
        day.get(VENDOR),
        Some(&VendorDay {
            orders: 2,
            gross: 242_000,
            currency_code: "VND".to_owned(),
        })
    );
    assert_eq!(day.len(), 1, "a walk-in is no vendor's");
    assert!(
        kept.orders.is_empty() && kept.bills.is_empty(),
        "nothing left waiting"
    );
}

#[test]
fn the_orders_waiting_for_a_settle_are_bounded() {
    let mut kept = KeptVendors::default();
    for order in 0..(MAX_WAITING as u128 + 10) {
        fold_vendors(&mut kept, &opened(order + 1, Some(VENDOR)));
    }
    assert_eq!(kept.orders.len(), MAX_WAITING);
    assert!(!kept.orders.contains_key(&id(1)), "the oldest went first");
}

fn voided(bill: u128) -> EventEnvelope<RawPayload> {
    event(
        EventType::BillingBillVoided,
        &json!({ "bill_id": id(bill), "reason_code_id": id(99) }),
    )
}

fn the_day(kept: &KeptVendors) -> Option<VendorDay> {
    kept.days
        .values()
        .next()
        .and_then(|day| day.get(VENDOR).cloned())
}

/// A paid delivery bill voided after payment comes back out of the day it was counted on; a bill
/// voided before it was paid takes nothing out, and its order still counts when billed again.
#[test]
fn a_paid_bill_voided_later_comes_back_out_of_its_day() {
    let mut kept = KeptVendors::default();
    for event in [
        opened(1, Some(VENDOR)),
        bill(11, 1),
        settled(11, 132_000),
        opened(2, Some(VENDOR)),
        bill(12, 2),
        settled(12, 110_000),
        voided(11),
        opened(3, Some(VENDOR)),
        bill(13, 3),
        voided(13),
        bill(14, 3),
        settled(14, 50_000),
    ] {
        fold_vendors(&mut kept, &event);
    }
    assert_eq!(
        the_day(&kept),
        Some(VendorDay {
            orders: 2,
            gross: 160_000,
            currency_code: "VND".to_owned(),
        }),
        "order 1 taken back out; order 3 counted once, at its second bill"
    );
    assert!(kept.settled.contains_key(&id(12)) && !kept.settled.contains_key(&id(11)));
}

/// Every part of a split delivery bill counts toward its vendor, and the order counts once, with
/// its last part; a part voided after payment takes only its own amount back out.
#[test]
fn every_part_of_a_split_bill_counts_and_the_order_counts_once() {
    let mut kept = KeptVendors::default();
    let split = event(
        EventType::BillingBillSplit,
        &json!({ "source_bill_id": id(11), "resulting_bill_ids": [id(21), id(22), id(23)] }),
    );
    for event in [
        opened(1, Some(VENDOR)),
        bill(11, 1),
        split,
        settled(21, 40_000),
    ] {
        fold_vendors(&mut kept, &event);
    }
    assert_eq!(
        the_day(&kept).map(|day| (day.orders, day.gross)),
        Some((0, 40_000))
    );
    for event in [settled(22, 50_000), settled(23, 42_000)] {
        fold_vendors(&mut kept, &event);
    }
    assert_eq!(
        the_day(&kept).map(|day| (day.orders, day.gross)),
        Some((1, 132_000))
    );
    fold_vendors(&mut kept, &voided(22));
    assert_eq!(
        the_day(&kept).map(|day| (day.orders, day.gross)),
        Some((1, 82_000))
    );
    fold_vendors(&mut kept, &voided(23));
    assert_eq!(
        the_day(&kept).map(|day| (day.orders, day.gross)),
        Some((0, 40_000)),
        "the part that counted the order takes it back out"
    );
}
