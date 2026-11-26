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
//! commission is applied when the day is read, at the rate the store's configuration gives. An
//! order that ends unpaid is forgotten with its close, and both waiting maps are bounded.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use pos_proto::envelope::{EventEnvelope, RawPayload};
use pos_proto::events::{
    BillingBillOpened, BillingBillSettled, SalesOrderClosed, SalesOrderOpened,
};

/// The most orders or bills waiting for their settle that are kept; past it the oldest is dropped.
pub const MAX_WAITING: usize = 4_096;

/// One vendor's orders on one day.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct VendorDay {
    /// Settled orders.
    pub orders: u64,
    /// What they took, minor units, tax included.
    pub gross: i64,
    /// The currency of `gross`, ISO 4217.
    pub currency_code: String,
}

/// The vendors' days, and the orders and bills still waiting for a settle.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct KeptVendors {
    /// Each trading day's vendors, keyed by day and then by vendor id.
    #[serde(default)]
    pub days: BTreeMap<String, BTreeMap<String, VendorDay>>,
    /// The vendor of each open order that came through one, by order id.
    #[serde(default)]
    pub orders: BTreeMap<String, String>,
    /// The order of each open bill on such an order, by bill id.
    #[serde(default)]
    pub bills: BTreeMap<String, String>,
}

/// Inserts into a waiting map, dropping its oldest (smallest ULID) entry past [`MAX_WAITING`].
fn wait(map: &mut BTreeMap<String, String>, key: String, vendor: String) {
    map.insert(key, vendor);
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
                wait(
                    &mut kept.orders,
                    opened.order_id.to_string(),
                    vendor_id.to_string(),
                );
            }
        }
        "billing.bill.opened" => {
            let order = event
                .data
                .decode::<BillingBillOpened>()
                .ok()
                .map(|opened| (opened.bill_id.to_string(), opened.order_id.to_string()));
            if let Some((bill_id, order_id)) = order
                && kept.orders.contains_key(&order_id)
            {
                wait(&mut kept.bills, bill_id, order_id);
            }
        }
        "billing.bill.settled" => {
            if let Ok(settled) = event.data.decode::<BillingBillSettled>()
                && let Some(order_id) = kept.bills.remove(&settled.bill_id.to_string())
                && let Some(vendor) = kept.orders.remove(&order_id)
            {
                let day = kept
                    .days
                    .entry(event.business_date.to_string())
                    .or_default()
                    .entry(vendor)
                    .or_default();
                day.orders = day.orders.saturating_add(1);
                day.gross = day.gross.saturating_add(settled.total_due.amount_minor);
                settled
                    .total_due
                    .currency_code
                    .as_str()
                    .clone_into(&mut day.currency_code);
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
