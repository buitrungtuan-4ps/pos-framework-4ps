// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

use std::collections::BTreeMap;

use pos_contract_tests::fixtures;
use pos_proto::envelope::{EventEnvelope, EventTypeRef, RawPayload};
use pos_proto::events::EventType;
use pos_proto::ids::StoreId;
use pos_proto::ulid::Ulid;
use serde_json::{Value, json};

use super::{KeptChannels, fold_channels};
use crate::types::{ChannelSales, DailyRevenue};

const GRAB: &str = "01J0000000000000000000GRAB";
const CAPICHI: &str = "01J0000000000000000000CAP1";

fn id(n: u128) -> String {
    Ulid::from_u128(n).to_string()
}

fn money(minor: i64) -> Value {
    json!({ "currency_code": "VND", "amount_minor": minor })
}

fn event(kind: EventType, data: &Value) -> EventEnvelope<RawPayload> {
    let mut base = fixtures::activations(StoreId::new(Ulid::from_u128(1)), 1, 1).remove(0);
    base.event_type = EventTypeRef::from_known(kind);
    base.data = RawPayload::encode(data).expect("encode");
    base
}

fn opened(
    order: u128,
    channel: &str,
    table: bool,
    vendor: Option<&str>,
) -> EventEnvelope<RawPayload> {
    let mut data = json!({ "order_id": id(order), "channel": channel });
    if table {
        data["table_id"] = json!(id(900));
    }
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

fn paid(bill: u128, method: &str, amount: i64) -> EventEnvelope<RawPayload> {
    event(
        EventType::BillingPaymentCaptured,
        &json!({
            "bill_id": id(bill), "payment_id": id(bill + 5_000), "method": method,
            "outcome": "PAYMENT_OUTCOME_CAPTURED", "tendered": money(amount),
            "applied_to_bill": money(amount), "change_given": money(0), "tip_amount": money(0),
        }),
    )
}

fn settled(bill: u128, total: i64) -> EventEnvelope<RawPayload> {
    event(
        EventType::BillingBillSettled,
        &json!({
            "bill_id": id(bill), "receipt_number": 1, "subtotal": money(total),
            "reduction_total": money(0), "service_charge": money(0), "tax_total": money(0),
            "rounding_adjustment": money(0), "total_due": money(total),
        }),
    )
}

fn voided(bill: u128) -> EventEnvelope<RawPayload> {
    event(
        EventType::BillingBillVoided,
        &json!({ "bill_id": id(bill), "reason_code_id": id(77) }),
    )
}

fn line(
    source: &str,
    vendor: Option<&str>,
    order_type: &str,
    bills: u64,
    sales: i64,
) -> ChannelSales {
    ChannelSales {
        source: source.to_owned(),
        vendor_id: vendor.map(str::to_owned),
        order_type: order_type.to_owned(),
        bills,
        sales,
    }
}

fn fold(events: &[EventEnvelope<RawPayload>]) -> DailyRevenue {
    let mut kept = KeptChannels::default();
    let mut revenue = BTreeMap::new();
    for event in events {
        fold_channels(&mut kept, &mut revenue, event);
    }
    revenue.into_values().next().expect("one day")
}

/// ADR-0259 decisions 2 and 4: each bill joins the line of its order's channel, app and type.
#[test]
fn each_settled_bill_joins_its_orders_channel_and_type() {
    let day = fold(&[
        opened(1, "SALES_CHANNEL_DINE_IN", true, None),
        bill(11, 1),
        settled(11, 200_000),
        opened(2, "SALES_CHANNEL_TAKEAWAY", false, None),
        bill(12, 2),
        settled(12, 90_000),
        opened(3, "SALES_CHANNEL_QR", true, None),
        bill(13, 3),
        settled(13, 150_000),
        opened(4, "SALES_CHANNEL_DELIVERY", false, Some(GRAB)),
        bill(14, 4),
        settled(14, 188_000),
        opened(5, "SALES_CHANNEL_DELIVERY", false, Some(CAPICHI)),
        bill(15, 5),
        settled(15, 120_000),
    ]);
    assert_eq!(
        day.by_channel,
        vec![
            line(
                "SALE_SOURCE_APP",
                Some(CAPICHI),
                "ORDER_TYPE_DELIVERY",
                1,
                120_000
            ),
            line(
                "SALE_SOURCE_APP",
                Some(GRAB),
                "ORDER_TYPE_DELIVERY",
                1,
                188_000
            ),
            line("SALE_SOURCE_POS", None, "ORDER_TYPE_DINE_IN", 1, 200_000),
            line("SALE_SOURCE_POS", None, "ORDER_TYPE_TAKEAWAY", 1, 90_000),
            line("SALE_SOURCE_QR", None, "ORDER_TYPE_DINE_IN", 1, 150_000),
        ]
    );
}

/// ADR-0259 decision 5: an in-app payment is counted under its app; a bill voided after payment
/// leaves its line and its app's payments.
#[test]
fn an_in_app_payment_is_named_by_its_app_and_a_void_takes_the_bill_back_out() {
    let day = fold(&[
        opened(4, "SALES_CHANNEL_DELIVERY", false, Some(GRAB)),
        bill(14, 4),
        paid(14, "PAYMENT_METHOD_VENDOR", 188_000),
        settled(14, 188_000),
        opened(6, "SALES_CHANNEL_DELIVERY", false, Some(GRAB)),
        bill(16, 6),
        paid(16, "PAYMENT_METHOD_VENDOR", 50_000),
        settled(16, 50_000),
        voided(16),
        opened(2, "SALES_CHANNEL_TAKEAWAY", false, None),
        bill(12, 2),
        paid(12, "PAYMENT_METHOD_CASH", 90_000),
        settled(12, 90_000),
    ]);
    assert_eq!(day.by_app_tender.get(GRAB), Some(&188_000));
    assert_eq!(day.by_app_tender.len(), 1, "cash is no app's");
    assert_eq!(
        day.by_channel,
        vec![
            line(
                "SALE_SOURCE_APP",
                Some(GRAB),
                "ORDER_TYPE_DELIVERY",
                1,
                188_000
            ),
            line("SALE_SOURCE_POS", None, "ORDER_TYPE_TAKEAWAY", 1, 90_000),
        ]
    );
}

/// Every part of a split bill keeps its order's line; a bill of an order never seen open is not
/// recorded, so the lines still sum to the day's bills and sales.
#[test]
fn a_split_keeps_its_line_and_an_unseen_order_is_not_recorded() {
    let day = fold(&[
        opened(1, "SALES_CHANNEL_DINE_IN", true, None),
        bill(11, 1),
        event(
            EventType::BillingBillSplit,
            &json!({ "source_bill_id": id(11), "resulting_bill_ids": [id(21), id(22)] }),
        ),
        settled(21, 100_000),
        settled(22, 60_000),
        bill(30, 99),
        settled(30, 10_000),
    ]);
    assert_eq!(
        day.by_channel,
        vec![
            line("SALE_SOURCE_POS", None, "ORDER_TYPE_DINE_IN", 2, 160_000),
            line("SALE_SOURCE_POS", None, "ORDER_TYPE_UNSPECIFIED", 1, 10_000),
        ]
    );
}
