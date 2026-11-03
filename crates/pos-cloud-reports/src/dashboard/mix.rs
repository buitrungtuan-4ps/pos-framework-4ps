// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! A line's new quantity, carried into the product mix.
//!
//! `sales.order_line.added` puts a line's item, quantity and total into its day's mix
//! ([`fold_revenue`](super::rollup::fold_revenue)). `sales.order_line.updated` carries only the
//! line's new quantity and total, not its item, so the rollup keeps, for each open order, what each
//! of its lines last added to the mix, and on an update moves that item's ordered quantity and value
//! by the difference, on the day the line was added. An iced tea added at one and raised to two is
//! two in the mix, at the two-glass total, as it is on the bill. The fold stays one event at a time
//! ([ADR-0036](../../../../docs/adr/0036-materialised-rollups.md)), so the materialised rollup and
//! a fold of the whole log agree.
//!
//! Bounded by the store's open orders: an order's lines are let go when `sales.order.closed` says no
//! line of it can change again.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use pos_proto::envelope::{EventEnvelope, RawPayload};
use pos_proto::events::{SalesOrderClosed, SalesOrderLineAdded, SalesOrderLineUpdated};

use crate::types::DailyRevenue;

/// What one line last added to its day's mix.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct KeptLine {
    /// The trading day the line was added on, whose mix it counts in.
    pub business_date: String,
    /// The line's menu item, the mix's key.
    pub menu_item_id: String,
    /// Its quantity, in thousandths of a unit.
    pub qty_milli: i64,
    /// Its line total, minor units.
    pub value: i64,
}

/// The lines of each open order, keyed by order id and then by line id.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct KeptLines {
    /// Each open order's lines.
    #[serde(default)]
    pub orders: BTreeMap<String, BTreeMap<String, KeptLine>>,
}

/// Folds a line's update into its day's mix, and keeps what each line added so an update can.
/// Every other event is ignored, and a payload that fails to decode is skipped, as the other folds
/// skip one. A line the rollup never saw added, one added before the rollup kept lines, changes
/// nothing.
pub fn fold_line_updates(
    kept: &mut KeptLines,
    revenue: &mut BTreeMap<String, DailyRevenue>,
    event: &EventEnvelope<RawPayload>,
) {
    match event.event_type.as_str() {
        "sales.order_line.added" => {
            if let Ok(line) = event.data.decode::<SalesOrderLineAdded>() {
                kept.orders
                    .entry(line.order_id.to_string())
                    .or_default()
                    .insert(
                        line.order_line_id.to_string(),
                        KeptLine {
                            business_date: event.business_date.to_string(),
                            menu_item_id: line.menu_item_id.to_string(),
                            qty_milli: line.quantity.as_milli(),
                            value: line.line_total.amount_minor,
                        },
                    );
            }
        }
        "sales.order_line.updated" => {
            if let Ok(update) = event.data.decode::<SalesOrderLineUpdated>()
                && let Some(line) = kept
                    .orders
                    .get_mut(&update.order_id.to_string())
                    .and_then(|lines| lines.get_mut(&update.order_line_id.to_string()))
            {
                let qty_milli = update.quantity.as_milli();
                let value = update.line_total.amount_minor;
                if let Some(mix) = revenue
                    .get_mut(&line.business_date)
                    .and_then(|day| day.by_item.get_mut(&line.menu_item_id))
                {
                    mix.ordered_qty_milli = mix
                        .ordered_qty_milli
                        .saturating_add(qty_milli.saturating_sub(line.qty_milli));
                    mix.ordered_value = mix
                        .ordered_value
                        .saturating_add(value.saturating_sub(line.value));
                }
                line.qty_milli = qty_milli;
                line.value = value;
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

#[cfg(test)]
mod tests;
