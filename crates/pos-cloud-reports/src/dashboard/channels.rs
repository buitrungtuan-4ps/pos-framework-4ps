// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! What a day's bills took by channel and order type, and what each delivery app's in-app payments
//! came to
//! ([ADR-0259](../../../../docs/adr/0259-a-sale-is-reported-by-its-channel-and-its-order-type.md)
//! decisions 4 and 5).
//!
//! One event at a time ([ADR-0036](../../../../docs/adr/0036-materialised-rollups.md)): an order's
//! channel is read off its `sales.order.opened` by [`sale_channel`], the function the edge calls
//! too, and remembered by the order's id; each bill by its own; a settle adds one bill and its total
//! to its line of the day, and a bill whose order was never seen open adds to
//! [`SaleChannel::NOT_RECORDED`]'s, so the lines always sum to the day's bills and sales. A captured
//! `PAYMENT_METHOD_VENDOR` payment adds to its app on the day it was captured, as `by_tender` does. A
//! bill voided after payment comes back out of both. Every kept map is bounded.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use pos_proto::envelope::{EventEnvelope, RawPayload};
use pos_proto::events::{
    BillingBillMerged, BillingBillOpened, BillingBillSettled, BillingBillSplit, BillingBillVoided,
    BillingPaymentCaptured, SalesOrderClosed, SalesOrderOpened,
};
use pos_proto::sale_channel::{Opening, SaleChannel, sale_channel};
use pos_proto::{PaymentMethod, PaymentOutcome};

use super::rollup::{empty_revenue, set_currency};
use super::vendors::MAX_WAITING;
use crate::types::{ChannelSales, DailyRevenue};

/// A line's key: its source token, its app's id, its order type token.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct LineKey {
    /// `SALE_SOURCE_*`.
    pub source: String,
    /// The app's connection id, for `SALE_SOURCE_APP`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vendor_id: Option<String>,
    /// `ORDER_TYPE_*`.
    pub order_type: String,
}

impl From<SaleChannel> for LineKey {
    fn from(channel: SaleChannel) -> Self {
        Self {
            source: channel.source.as_str().to_owned(),
            vendor_id: channel.vendor_id.map(|vendor| vendor.to_string()),
            order_type: channel.order_type.as_str().to_owned(),
        }
    }
}

/// An in-app payment: the day it was captured, its app, what it applied.
pub type AppPayment = (String, String, i64);

/// A settled bill, kept so a void after payment takes it back out.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SettledLine {
    /// The day it was counted on.
    pub day: String,
    /// Its line.
    pub key: LineKey,
    /// What it took.
    pub total: i64,
    /// Its in-app payments.
    #[serde(default)]
    pub app_payments: Vec<AppPayment>,
}

/// The orders, bills and payments a later event still needs.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct KeptChannels {
    /// Each order seen open and not yet paid in full or closed, by order id.
    #[serde(default)]
    pub orders: BTreeMap<String, LineKey>,
    /// The order of each open bill, by bill id.
    #[serde(default)]
    pub bills: BTreeMap<String, String>,
    /// In-app payments on a bill whose settle has not been folded yet, by bill id.
    #[serde(default)]
    pub pending: BTreeMap<String, Vec<AppPayment>>,
    /// Each settled bill, by bill id, until it is voided or ages out.
    #[serde(default)]
    pub settled: BTreeMap<String, SettledLine>,
}

/// Inserts into a kept map, dropping its oldest (smallest ULID) entry past [`MAX_WAITING`].
fn keep<V>(map: &mut BTreeMap<String, V>, key: String, value: V) {
    map.insert(key, value);
    while map.len() > MAX_WAITING {
        map.pop_first();
    }
}

/// Folds one event into the days' channel lines and in-app payments. Every other event is ignored,
/// and a payload that fails to decode is skipped, as the other folds skip one.
pub fn fold_channels(
    kept: &mut KeptChannels,
    revenue: &mut BTreeMap<String, DailyRevenue>,
    event: &EventEnvelope<RawPayload>,
) {
    match event.event_type.as_str() {
        "sales.order.opened" => {
            if let Ok(opened) = event.data.decode::<SalesOrderOpened>() {
                let channel = sale_channel(Opening {
                    channel: opened.channel.require().ok(),
                    table_id: opened.table_id,
                    vendor_id: opened.vendor_id,
                    order_type: opened.order_type.as_ref(),
                    sale_source: opened.sale_source.as_ref(),
                });
                keep(
                    &mut kept.orders,
                    opened.order_id.to_string(),
                    channel.into(),
                );
            }
        }
        "billing.bill.opened" => {
            if let Ok(opened) = event.data.decode::<BillingBillOpened>() {
                keep(
                    &mut kept.bills,
                    opened.bill_id.to_string(),
                    opened.order_id.to_string(),
                );
            }
        }
        "billing.bill.split" => {
            if let Ok(split) = event.data.decode::<BillingBillSplit>()
                && let Some(order_id) = kept.bills.remove(&split.source_bill_id.to_string())
            {
                for part in split.resulting_bill_ids {
                    keep(&mut kept.bills, part.to_string(), order_id.clone());
                }
            }
        }
        "billing.bill.merged" => {
            if let Ok(merged) = event.data.decode::<BillingBillMerged>() {
                for bill in &merged.merged_bill_ids {
                    kept.bills.remove(&bill.to_string());
                }
            }
        }
        "billing.payment.captured" => {
            if let Ok(payment) = event.data.decode::<BillingPaymentCaptured>() {
                capture(kept, revenue, event, &payment);
            }
        }
        "billing.bill.settled" => {
            if let Ok(settled) = event.data.decode::<BillingBillSettled>() {
                settle(kept, revenue, event.business_date.to_string(), &settled);
            }
        }
        "billing.bill.voided" => {
            if let Ok(voided) = event.data.decode::<BillingBillVoided>() {
                void(kept, revenue, &voided.bill_id.to_string());
            }
        }
        "sales.order.closed" => {
            if let Ok(closed) = event.data.decode::<SalesOrderClosed>() {
                kept.orders.remove(&closed.order_id.to_string());
            }
        }
        _ => {}
    }
}

/// The line of `bill_id`'s order, or the not-recorded line for an order never seen open.
fn line_of(kept: &KeptChannels, bill_id: &str) -> LineKey {
    kept.bills
        .get(bill_id)
        .and_then(|order_id| kept.orders.get(order_id))
        .cloned()
        .unwrap_or_else(|| SaleChannel::NOT_RECORDED.into())
}

/// The day's revenue, created empty where it is not yet.
fn day_of<'a>(revenue: &'a mut BTreeMap<String, DailyRevenue>, day: &str) -> &'a mut DailyRevenue {
    revenue
        .entry(day.to_owned())
        .or_insert_with(|| empty_revenue(day))
}

/// Adds a captured in-app payment to its app on the day it was captured, and keeps it on its bill.
fn capture(
    kept: &mut KeptChannels,
    revenue: &mut BTreeMap<String, DailyRevenue>,
    event: &EventEnvelope<RawPayload>,
    payment: &BillingPaymentCaptured,
) {
    if payment.outcome.known() != PaymentOutcome::Captured
        || payment.method.known() != PaymentMethod::Vendor
    {
        return;
    }
    let bill_id = payment.bill_id.to_string();
    let Some(vendor) = line_of(kept, &bill_id).vendor_id else {
        return;
    };
    let date = event.business_date.to_string();
    let applied = payment.applied_to_bill;
    let day = day_of(revenue, &date);
    set_currency(&mut day.currency_code, applied.currency_code.to_string());
    let slot = day.by_app_tender.entry(vendor.clone()).or_insert(0);
    *slot = slot.saturating_add(applied.amount_minor);
    let pending = kept.pending.entry(bill_id.clone()).or_default();
    pending.push((date, vendor, applied.amount_minor));
    if kept.pending.len() > MAX_WAITING {
        kept.pending.pop_first();
    }
}

/// Adds `bills` and `sales` to `key`'s line of `day`, kept in order of source, app and type.
fn add_line(day: &mut DailyRevenue, key: &LineKey, bills: i64, sales: i64) {
    let wanted = (&key.source, &key.vendor_id, &key.order_type);
    let at = day
        .by_channel
        .binary_search_by(|line| (&line.source, &line.vendor_id, &line.order_type).cmp(&wanted));
    let index = at.unwrap_or_else(|index| {
        day.by_channel.insert(
            index,
            ChannelSales {
                source: key.source.clone(),
                vendor_id: key.vendor_id.clone(),
                order_type: key.order_type.clone(),
                bills: 0,
                sales: 0,
            },
        );
        index
    });
    if let Some(line) = day.by_channel.get_mut(index) {
        line.bills = line.bills.saturating_add_signed(bills);
        line.sales = line.sales.saturating_add(sales);
        // A line every bill of which was voided is no line.
        if line.bills == 0 && line.sales == 0 {
            day.by_channel.remove(index);
        }
    }
}

/// Adds a settled bill to its line of `day`; its order is forgotten once no bill of it is open.
fn settle(
    kept: &mut KeptChannels,
    revenue: &mut BTreeMap<String, DailyRevenue>,
    day: String,
    settled: &BillingBillSettled,
) {
    let bill_id = settled.bill_id.to_string();
    let key = line_of(kept, &bill_id);
    if let Some(order_id) = kept.bills.remove(&bill_id)
        && !kept.bills.values().any(|other| *other == order_id)
    {
        kept.orders.remove(&order_id);
    }
    let total = settled.total_due.amount_minor;
    add_line(day_of(revenue, &day), &key, 1, total);
    let app_payments = kept.pending.remove(&bill_id).unwrap_or_default();
    keep(
        &mut kept.settled,
        bill_id,
        SettledLine {
            day,
            key,
            total,
            app_payments,
        },
    );
}

/// Takes a bill voided after payment back out of its line, and its in-app payments out of their
/// days; a bill voided before it settled gives back only the in-app payments it took.
fn void(kept: &mut KeptChannels, revenue: &mut BTreeMap<String, DailyRevenue>, bill_id: &str) {
    kept.bills.remove(bill_id);
    let payments = match kept.settled.remove(bill_id) {
        Some(part) => {
            add_line(day_of(revenue, &part.day), &part.key, -1, -part.total);
            part.app_payments
        }
        None => kept.pending.remove(bill_id).unwrap_or_default(),
    };
    for (date, vendor, amount) in payments {
        if let Some(slot) = revenue
            .get_mut(&date)
            .and_then(|day| day.by_app_tender.get_mut(&vendor))
        {
            *slot = slot.saturating_sub(amount);
        }
    }
}

#[cfg(test)]
mod shared_day;
#[cfg(test)]
mod tests;
