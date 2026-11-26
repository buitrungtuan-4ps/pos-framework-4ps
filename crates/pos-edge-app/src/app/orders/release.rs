// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! How a guest's or a vendor's order reaches the kitchen
//! ([ADR-0247](../../../../../docs/adr/0247-a-guest-or-vendor-order-reaches-the-kitchen-as-the-store-chooses.md)):
//! the fires an order accepted on arrival writes with itself, under
//! `KITCHEN_RELEASE_AUTO_ACCEPT_AND_FIRE`. Confirming under `KITCHEN_RELEASE_CONFIRM_FIRES` fires
//! through the kitchen's own [`Edge::prepare_fires`], under the person who confirms.

use pos_core::decision::{Actor, DecisionCtx, LineCommand, decide_line};
use pos_core::permission::{Permission, PermissionSet};
use pos_ports::event_store::EventStore;
use pos_proto::events::SalesOrderLineFired;
use pos_proto::ids::{DeviceId, EmployeeId, OrderLineId};
use pos_proto::time::{BusinessDate, Timestamp};

use crate::app::kitchen::PreparedFires;
use pos_proto::OrderLineState;
use pos_proto::ids::OrderId;

use crate::app::{Edge, FiredLine, LineRecord, LineView};

impl<S: EventStore> Edge<S> {
    /// The fires of an order accepted on arrival: each line of `records` to the station the
    /// published plan routes it to, decided as any fire is, and stamped by the box that took the
    /// order, never a person (ADR-0247 decision 4). The context holds only `sales.line.fire`.
    ///
    /// `None` when a line routes to no station or may not fire: the order is then taken as it was
    /// before the setting, unheld and unsent, for staff to send, and that is logged.
    pub(in crate::app) fn intake_fires(
        &self,
        device_id: DeviceId,
        now: Timestamp,
        business_date: BusinessDate,
        records: &[(OrderLineId, LineRecord)],
    ) -> Option<(DecisionCtx, PreparedFires)> {
        let session = self.session();
        let grants = PermissionSet::EMPTY.with(Permission::FireLines);
        let ctx = DecisionCtx {
            now,
            business_date,
            // The box acting for itself: the envelope names the device and no employee.
            actor: Actor {
                employee_id: EmployeeId::new(device_id.as_ulid()),
                device_id,
            },
            granted: grants,
            granted_directly: grants,
            capabilities: session.capabilities,
            connectivity: session.connectivity,
            currency: session.currency,
        };
        let mut envelopes = Vec::with_capacity(records.len());
        let mut messages = Vec::with_capacity(records.len());
        let mut applied = Vec::with_capacity(records.len());
        for (order_line_id, record) in records {
            let fired = session
                .resolve_station(record.menu_item_id, record.course_id)
                .and_then(|station_id| {
                    let command = LineCommand::Fire {
                        base_item: record.menu_item_id,
                        modifiers: record.modifier_menu_item_ids.clone(),
                        quantity: record.quantity,
                        course: None,
                    };
                    let decision =
                        decide_line(record.state, command, &ctx, &session.recipes).ok()?;
                    let event = SalesOrderLineFired {
                        order_id: record.order_id,
                        order_line_id: *order_line_id,
                        station_id,
                        fire_time: now,
                    };
                    let prepared = self
                        .system_prepare(device_id, now, business_date, &event)
                        .ok()?;
                    Some((prepared, station_id, decision))
                });
            let Some((prepared, station_id, decision)) = fired else {
                tracing::warn!(
                    order_id = %record.order_id,
                    "an order taken on arrival could not be sent to the kitchen; staff send it"
                );
                return None;
            };
            let (envelope, message) = prepared;
            envelopes.push(envelope);
            messages.push(message);
            applied.push((*order_line_id, record.clone(), station_id, decision));
        }
        Some((ctx, (envelopes, messages, applied)))
    }

    /// The lines of `order_id` already sent to the kitchen, each with what its ticket says: for the
    /// tickets of an order accepted and sent on arrival (ADR-0247), which no person's send prints.
    /// A ticket's job id is its line's, so printing a line twice prints one ticket.
    #[must_use]
    pub fn sent_lines(&self, order_id: OrderId) -> Vec<LineView> {
        self.lock_projection()
            .identified_lines_for_order(order_id)
            .into_iter()
            .filter(|(_, record)| record.state == OrderLineState::Fired)
            .filter_map(|(order_line_id, record)| {
                let station_id = record.station_id?;
                Some(LineView {
                    order_id: record.order_id,
                    order_line_id,
                    state: record.state,
                    fired: Some(FiredLine {
                        station_id,
                        menu_item_id: record.menu_item_id,
                        quantity: record.quantity,
                        modifier_menu_item_ids: record.modifier_menu_item_ids,
                        note_present: record.note_present,
                    }),
                })
            })
            .collect()
    }
}
