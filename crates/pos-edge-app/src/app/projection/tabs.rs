// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! What the projection records of bar tabs: each open tab's number (ADR-0221).

use pos_proto::OrderLineState;
use pos_proto::ids::{BillId, OrderId};
use pos_proto::time::Timestamp;

use crate::app::Projection;

impl Projection {
    /// Records the number a tab opened with, live and on rebuild; an order that is no tab records
    /// nothing.
    pub(in crate::app) fn record_tab(&mut self, order_id: OrderId, tab_number: Option<u64>) {
        if let Some(number) = tab_number {
            self.tabs.insert(order_id, number);
        }
    }

    /// Every open tab not moved to a table, by number: its order, number, the newest fire among
    /// its lines (its last round) and any bill open on it.
    pub(in crate::app) fn open_tabs(
        &self,
    ) -> Vec<(OrderId, u64, Option<Timestamp>, Option<BillId>)> {
        let mut tabs: Vec<_> = self
            .tabs
            .iter()
            .filter(|(order_id, _)| self.table_for_order(**order_id).is_none())
            .map(|(order_id, number)| {
                let last_round_time = self
                    .line_ids_for_order(*order_id)
                    .iter()
                    .filter_map(|line_id| self.lines.get(line_id))
                    .filter(|record| record.state != OrderLineState::Voided)
                    .filter_map(|record| record.fired_time)
                    .max();
                (
                    *order_id,
                    *number,
                    last_round_time,
                    self.bill_for_order(*order_id),
                )
            })
            .collect();
        tabs.sort_by_key(|(_, number, _, _)| *number);
        tabs
    }
}
