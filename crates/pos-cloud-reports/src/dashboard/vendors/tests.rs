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
