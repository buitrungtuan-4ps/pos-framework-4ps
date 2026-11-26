// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! What each delivery vendor's orders took on a day, for the reconciliation with what the vendor
//! pays out
//! ([ADR-0248](../../../../docs/adr/0248-a-delivery-order-is-priced-paid-and-reconciled-as-the-app-sold-it.md)
//! decision 3).
//!
//! One event at a time ([ADR-0036](../../../../docs/adr/0036-materialised-rollups.md)): an order
//! opened through a vendor is remembered by its id, its bill by the bill's id, and the bill's
//! settle adds one order and its total to the vendor's day. The bill and its tax stay gross; the
//! commission is applied when the day is read, at the rate the store's configuration gives. Every
//! part of a split bill counts, and the order counts once, with its last part; a settled bill voided
//! after payment comes back out of the day it was counted on. An order that ends unpaid is forgotten
//! with its close, and every kept map is bounded.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use pos_proto::envelope::{EventEnvelope, RawPayload};
use pos_proto::events::{
    BillingBillMerged, BillingBillOpened, BillingBillSettled, BillingBillSplit, BillingBillVoided,
    SalesOrderClosed, SalesOrderOpened,
};

/// The most orders, open bills or settled bills each map keeps; past it the oldest goes first.
pub const MAX_WAITING: usize = 4_096;

/// One vendor's orders on one day.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct VendorDay {
    /// Settled orders: an order counts once, when its last open part settles.
    pub orders: u64,
    /// What they took, minor units, tax included: every part of each.
    pub gross: i64,
    /// The currency of `gross`, ISO 4217.
    pub currency_code: String,
}

/// A settled bill of a vendor's order, kept so a void after payment takes it back out.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SettledPart {
    /// The trading day it was counted on.
    pub day: String,
    /// Its vendor.
    pub vendor: String,
    /// What it took.
    pub total: i64,
    /// Whether its settle counted the order.
    pub counted_order: bool,
}

/// The vendors' days, and the orders and bills a later event still needs.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct KeptVendors {
    /// Each trading day's vendors, keyed by day and then by vendor id.
    #[serde(default)]
    pub days: BTreeMap<String, BTreeMap<String, VendorDay>>,
    /// The vendor of each order that came through one and is not yet paid in full, by order id.
    #[serde(default)]
    pub orders: BTreeMap<String, String>,
    /// The order of each open bill on such an order, by bill id: a split's parts each.
    #[serde(default)]
    pub bills: BTreeMap<String, String>,
    /// Each settled bill of such an order, by bill id, until it is voided or ages out.
    #[serde(default)]
    pub settled: BTreeMap<String, SettledPart>,
}

/// Inserts into a kept map, dropping its oldest (smallest ULID) entry past [`MAX_WAITING`].
fn keep<V>(map: &mut BTreeMap<String, V>, key: String, value: V) {
    map.insert(key, value);
    while map.len() > MAX_WAITING {
        map.pop_first();
    }
}

/// Folds one event into the vendors' days. Every other event is ignored, and a payload that fails
/// to decode is skipped, as the other folds skip one.
pub fn fold_vendors(kept: &mut KeptVendors, event: &EventEnvelope<RawPayload>) {
    match event.event_type.as_str() {
        "sales.order.opened" => {
            if let Ok(opened) = event.data.decode::<SalesOrderOpened>()
                && let Some(vendor_id) = opened.vendor_id
            {
                keep(
                    &mut kept.orders,
                    opened.order_id.to_string(),
                    vendor_id.to_string(),
                );
            }
        }
        "billing.bill.opened" => {
            if let Ok(opened) = event.data.decode::<BillingBillOpened>() {
                let order_id = opened.order_id.to_string();
                if kept.orders.contains_key(&order_id) {
                    keep(&mut kept.bills, opened.bill_id.to_string(), order_id);
                }
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
                let order = merged
                    .merged_bill_ids
                    .iter()
                    .filter_map(|bill| kept.bills.remove(&bill.to_string()))
                    .last();
                if let Some(order_id) = order {
                    keep(&mut kept.bills, merged.target_bill_id.to_string(), order_id);
                }
            }
        }
        "billing.bill.settled" => {
            if let Ok(settled) = event.data.decode::<BillingBillSettled>() {
                settle(kept, event.business_date.to_string(), &settled);
            }
        }
        "billing.bill.voided" => {
            if let Ok(voided) = event.data.decode::<BillingBillVoided>() {
                void(kept, &voided.bill_id.to_string());
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

/// Adds a settled part to its vendor's day; the order counts once its last open part has settled.
fn settle(kept: &mut KeptVendors, day: String, settled: &BillingBillSettled) {
    let bill_id = settled.bill_id.to_string();
    let Some(order_id) = kept.bills.remove(&bill_id) else {
        return;
    };
    let Some(vendor) = kept.orders.get(&order_id).cloned() else {
        return;
    };
    let last = !kept.bills.values().any(|other| *other == order_id);
    if last {
        kept.orders.remove(&order_id);
    }
    let total = settled.total_due.amount_minor;
    let entry = kept
        .days
        .entry(day.clone())
        .or_default()
        .entry(vendor.clone())
        .or_default();
    entry.orders = entry.orders.saturating_add(u64::from(last));
    entry.gross = entry.gross.saturating_add(total);
    settled
        .total_due
        .currency_code
        .as_str()
        .clone_into(&mut entry.currency_code);
    keep(
        &mut kept.settled,
        bill_id,
        SettledPart {
            day,
            vendor,
            total,
            counted_order: last,
        },
    );
}

/// Takes a settled part voided after payment back out of the day it was counted on. A bill voided
/// before it was paid is only forgotten: its order may yet be billed again.
fn void(kept: &mut KeptVendors, bill_id: &str) {
    kept.bills.remove(bill_id);
    let Some(part) = kept.settled.remove(bill_id) else {
        return;
    };
    if let Some(entry) = kept
        .days
        .get_mut(&part.day)
        .and_then(|vendors| vendors.get_mut(&part.vendor))
    {
        entry.gross = entry.gross.saturating_sub(part.total);
        entry.orders = entry.orders.saturating_sub(u64::from(part.counted_order));
    }
}

/// The vendors' days in `window`, oldest day first, each day with its vendors: the days the revenue
/// read would keep for the same window, so the two reads always cover the same days.
#[must_use]
pub fn render_window(
    mut days: BTreeMap<String, BTreeMap<String, VendorDay>>,
    window: &super::RollupWindow,
) -> Vec<(String, BTreeMap<String, VendorDay>)> {
    let dated = days
        .keys()
        .map(|day| (day.clone(), super::rollup::empty_revenue(day)))
        .collect();
    super::rollup::render_revenue_window(dated, window)
        .into_iter()
        .filter_map(|kept| {
            let vendors = days.remove(&kept.business_date)?;
            Some((kept.business_date, vendors))
        })
        .collect()
}

#[cfg(test)]
mod tests;
