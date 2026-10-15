// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! Bumping a ticket when it is ready, and the live orders the floor and the kitchen display read.

use pos_core::business_date::derive_business_date;
use pos_core::decision::Actor;
use pos_core::permission::Permission;
use pos_ports::event_store::EventStore;
use pos_proto::ClockSource;
use pos_proto::events::KitchenTicketBumped;
use pos_proto::ids::{OrderId, OrderLineId, StationId};
use pos_proto::text::DisplayName;

use crate::app::{
    AppError, BumpView, Edge, LiveOrderLine, LiveOrderView, LiveSnapshot, Projection,
};

impl<S: EventStore> Edge<S> {
    /// Bumps a kitchen ticket — a station marks lines prepared (`kitchen.ticket.bumped`,
    /// [ADR-0026](../../../../../docs/adr/0026-port-shapes.md) event catalogue §18). Durable and fanned out,
    /// so a second KDS agrees the ticket is done rather than each screen holding a private, divergent
    /// "done" flag. A bump is orthogonal to a line's order state (a made line is still "fired"), so it
    /// is recorded on the projection's bumped set, not as a state-machine transition.
    ///
    /// # Errors
    ///
    /// [`AppError::Domain`] if the actor lacks `sales.ticket.bump`, or [`AppError`] if the business
    /// date cannot be derived, the event cannot be encoded, or the store cannot be written.
    pub async fn bump_ticket(
        &self,
        actor: Actor,
        order_id: OrderId,
        station_id: StationId,
        order_line_ids: Vec<OrderLineId>,
    ) -> Result<BumpView, AppError> {
        let ctx = self.decision_ctx(actor)?;
        ctx.require(Permission::BumpTicket)?;
        let payload = KitchenTicketBumped {
            order_id,
            station_id,
            order_line_ids: order_line_ids.clone(),
        };
        self.commit_and_publish(&ctx, &payload).await?;
        self.lock_projection().mark_bumped(&order_line_ids);
        // A paid order's note is kept for the kitchen only until the line is made (ADR-0161).
        self.forget_notes_off_screen();
        Ok(BumpView {
            order_id,
            station_id,
            order_line_ids,
        })
    }

    /// Every open order with its lines — what a device reads to rebuild its screens.
    ///
    /// The gap this closes: a device learns lines from the fan-out, and the fan-out carries what
    /// happens next. A till that reloaded, a tablet that slept, a kitchen display switched on at
    /// five o'clock — each drew an empty order and an empty board while the food existed, because
    /// nothing it could ask answered "what is open right now". `loadStore` read the floor, the price
    /// book, the button plan and the money settings; this is the fifth read it was missing.
    ///
    /// One read for every screen that needs it, keyed on the order: the floor's orders and the
    /// counter's both appear, because the kitchen works from both and a counter order sits on no
    /// table ([ADR-0093](../../../../../docs/adr/0093-bill-keyed-on-order.md)).
    ///
    /// The list is the store's **live trading**, not its log: a settled order drops out of
    /// `Projection::open_orders`, so what this returns is bounded by the tables the store has
    /// open plus the counter orders it has not yet taken money for.
    ///
    /// # Panics
    ///
    /// If the projection lock is poisoned — unreachable, as its critical section only reads.
    #[must_use]
    pub fn live_orders(&self) -> Vec<LiveOrderView> {
        // Copied out under one guard, so the answer is one consistent moment across every order,
        // and assembled after it is released: the menu lookups and the allocation are the slow part,
        // and every sale waits on this lock (F2).
        let snapshot = {
            let projection = self.lock_projection();
            Self::live_snapshot_of(&projection, projection.open_orders())
        };
        self.views_of(snapshot)
    }

    /// What the kitchen still has to make
    /// ([ADR-0161](../../../../../docs/adr/0161-a-paid-order-without-a-table-stays-on-the-kitchen-board-until-it-is-done.md)):
    /// every open order, as [`Self::live_orders`] has them, and each paid order without a table that
    /// still has a fired line no station has bumped, fired on the current business day.
    ///
    /// A counter order is paid before it is cooked, and the live read drops an order the moment it
    /// is paid, so a kitchen display that reloaded after the payment lost the ticket and its note.
    /// The day's end is applied here, where the clock is: a paid order fired on an earlier business
    /// day is dropped from the projection's waiting set, and its notes are forgotten with it.
    #[must_use]
    pub fn kitchen_orders(&self) -> Vec<LiveOrderView> {
        let session = self.session();
        let today = derive_business_date(self.clock.now(), &session.timezone, session.cutoff);
        let snapshot = {
            let mut projection = self.lock_projection();
            if let Ok(today) = today {
                projection.cooking.retain(|_, fired| {
                    derive_business_date(*fired, &session.timezone, session.cutoff)
                        .is_ok_and(|day| day == today)
                });
            }
            let waiting: Vec<OrderId> = projection.cooking.keys().copied().collect();
            let mut orders = projection.open_orders();
            orders.extend(waiting);
            Self::live_snapshot_of(&projection, orders)
        };
        self.forget_notes_off_screen();
        self.views_of(snapshot)
    }

    /// The orders `order_ids` names, with their lines, copied out under the caller's guard.
    fn live_snapshot_of(projection: &Projection, order_ids: Vec<OrderId>) -> Vec<LiveSnapshot> {
        order_ids
            .into_iter()
            .map(|order_id| LiveSnapshot {
                order_id,
                table_id: projection.table_for_order(order_id),
                bill_id: projection.bill_for_order(order_id),
                open_bill_ids: projection.open_bills_for_order(order_id),
                sales_channel: projection.order_channel(order_id),
                lines: projection
                    .identified_lines_for_order(order_id)
                    .into_iter()
                    .map(|(id, record)| {
                        let bumped = projection.bumped_lines.contains(&id);
                        (id, record, bumped)
                    })
                    .collect(),
            })
            .collect()
    }

    /// A snapshot as the screens read it: names from the menu, and each line's note.
    fn views_of(&self, snapshot: Vec<LiveSnapshot>) -> Vec<LiveOrderView> {
        let session = self.session();
        // After the projection's guard is released: the two locks are never held together.
        let notes = self.lock_notes();
        snapshot
            .into_iter()
            .map(|order| LiveOrderView {
                order_id: order.order_id,
                table_id: order.table_id,
                bill_id: order.bill_id,
                open_bill_ids: order.open_bill_ids,
                sales_channel: order.sales_channel,
                lines: order
                    .lines
                    .into_iter()
                    .map(|(order_line_id, record, bumped)| LiveOrderLine {
                        order_line_id,
                        display_name: session.menu_entry(record.menu_item_id).map_or_else(
                            || DisplayName::new(record.menu_item_id.to_string()),
                            |entry| entry.display_name.clone(),
                        ),
                        quantity: record.quantity,
                        line_total: record.line_total,
                        state: record.state,
                        bumped,
                        fired_time: record.fired_time,
                        modifier_menu_item_ids: record.modifier_menu_item_ids,
                        seat: record.seat,
                        course_id: record.course_id,
                        station_id: record.station_id,
                        note_present: record.note_present,
                        note: notes.get(order_line_id).cloned(),
                    })
                    .collect(),
            })
            .collect()
    }
}
