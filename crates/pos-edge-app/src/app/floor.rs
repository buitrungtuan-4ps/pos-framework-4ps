// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The floor: a table's state and how long it has been seated, seating, cleaning and releasing it,
//! moving its guests to another table, and the order a table holds.

use pos_core::decision::{Actor, TableCommand, decide_table};
use pos_ports::event_store::EventStore;
use pos_proto::events::{
    SalesOrderClosed, SalesOrderOpened, SalesTableClosed, SalesTableOpened, SalesTableTransferred,
};
use pos_proto::ids::{OrderId, TableId};
use pos_proto::time::Timestamp;
use pos_proto::{BillState, Open, SalesChannel, TableState};

use crate::app::{AppError, Edge, TableView, TransferView};

impl<S: EventStore> Edge<S> {
    /// When the guests at a table sat down, or `None` when nobody is sitting there.
    ///
    /// Read from the id of the order the table holds — a ULID minted at the moment it was seated,
    /// the reading [ADR-0145](../../../../docs/adr/0145-the-edge-keeps-events-until-synced-and-n-days-old.md)
    /// makes for retention — rather than from a time stored beside it. Only while somebody is
    /// sitting there, to be served or to pay: a table waiting to be cleaned has been left.
    #[must_use]
    pub fn seated_since(&self, table_id: TableId) -> Option<Timestamp> {
        if !matches!(
            self.table_state(table_id),
            TableState::Occupied | TableState::AwaitingPayment
        ) {
            return None;
        }
        let order_id = self.lock_projection().order_for_table(table_id)?;
        let millis = i64::try_from(order_id.as_ulid().timestamp_ms()).ok()?;
        Timestamp::from_milliseconds_since_epoch(millis).ok()
    }

    /// The current projected state of a table.
    #[must_use]
    pub fn table_state(&self, table_id: TableId) -> TableState {
        self.projection
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .table_state(table_id)
    }

    /// Seats guests at a table, opening an order on it (`sales.table.opened` **and**
    /// `sales.order.opened`).
    ///
    /// # Two events, not one
    ///
    /// Until roadmap **B1.2** this emitted only `sales.table.opened`, so a dine-in order existed
    /// with no record of the channel it came in on and no guest count — and the bill was taxed at
    /// whatever `EdgeSession.sales_channel` happened to be store-wide. The channel is
    /// [`SalesChannel::DineIn`] by construction here: seating a table *is* dine-in, whatever the
    /// session says, which is why it is not a parameter.
    ///
    /// `guest_count` is optional because staff may not record it and a missing count must never
    /// stop a table being seated. It is reporting data (average check per cover), not a gate.
    ///
    /// # Errors
    ///
    /// [`AppError::OpenShiftRequired`] while no shift is open at a store that refuses selling
    /// without one, or [`AppError`] if the transition is illegal, the store cannot be written, or
    /// the business date cannot be derived.
    pub async fn seat_table(
        &self,
        actor: Actor,
        table_id: TableId,
        guest_count: Option<u16>,
    ) -> Result<TableView, AppError> {
        // A replaced machine seats nobody new (ADR-0123). The tables already seated on it finish
        // and settle here; this is what stops the floor refilling.
        self.refuse_if_superseded()?;
        self.refuse_without_open_shift()?;
        let ctx = self.decision_ctx(actor)?;
        let current = self.table_state(table_id);
        let decision = decide_table(current, TableCommand::Seat, &ctx)?;

        let order_id = self.next_order_id();
        let channel = SalesChannel::DineIn;
        // Both events in one transaction: a table occupied with no order behind it, or an order
        // with no channel, are each a state a screen would have to guess at.
        let table_opened = SalesTableOpened { table_id, order_id };
        let order_opened = SalesOrderOpened {
            order_id,
            channel: Open::from_known(channel),
            table_id: Some(table_id),
            guest_count,
        };
        let (table_envelope, table_message) = self.prepare(&ctx, &table_opened)?;
        let (order_envelope, order_message) = self.prepare(&ctx, &order_opened)?;
        self.append_and_publish(
            vec![table_envelope, order_envelope],
            vec![table_message, order_message],
        )
        .await?;

        // After the commit the change is durable, so it is safe to show and to remember.
        {
            let mut projection = self.lock_projection();
            projection.set_table(table_id, decision.next_state);
            projection.open_order(table_id, order_id);
            projection.open_order_origin(order_id, Some(channel), Some(table_id));
        }
        Ok(TableView {
            table_id,
            state: decision.next_state,
        })
    }

    /// Cleans a table down and releases it (`sales.table.closed`).
    ///
    /// # Errors
    ///
    /// [`AppError`] as [`Self::seat_table`].
    pub async fn clean_table(
        &self,
        actor: Actor,
        table_id: TableId,
    ) -> Result<TableView, AppError> {
        let ctx = self.decision_ctx(actor)?;
        let current = self.table_state(table_id);
        let decision = decide_table(current, TableCommand::Clean, &ctx)?;

        let payload = SalesTableClosed { table_id };
        self.commit_and_publish(&ctx, &payload).await?;
        self.lock_projection()
            .set_table(table_id, decision.next_state);
        Ok(TableView {
            table_id,
            state: decision.next_state,
        })
    }

    /// Releases a table seated by mistake, or whose guests left before anything was sold, straight
    /// back to service
    /// ([ADR-0163](../../../../docs/adr/0163-a-table-seated-by-mistake-is-released.md)):
    /// `sales.order.closed` for its order and `sales.table.closed`, in one transaction.
    ///
    /// Only while nothing is sold on it: every line voided or none at all, and no bill open. Before
    /// this a table seated by mistake could leave `Occupied` only by a bill on nothing, which could
    /// never settle, so it stayed taken until the store was re-provisioned. A table stuck that way
    /// already is recovered once: a manager voids the empty bill, and the table is released.
    ///
    /// # Errors
    ///
    /// [`AppError::OrderNotEmpty`] if something is sold on the table; [`AppError::Domain`] if the
    /// table is not seated, or the store does not do table service; or [`AppError`] if the store
    /// cannot be written.
    pub async fn release_table(
        &self,
        actor: Actor,
        table_id: TableId,
    ) -> Result<TableView, AppError> {
        let ctx = self.decision_ctx(actor)?;
        let decision = decide_table(self.table_state(table_id), TableCommand::Release, &ctx)?;
        let order_id = {
            let projection = self.lock_projection();
            let order_id = projection.order_for_table(table_id);
            if let Some(order_id) = order_id {
                let sold = !projection.sold_line_ids(order_id).is_empty();
                let billed = projection
                    .latest_bill_in(order_id, BillState::Open)
                    .is_some();
                if sold || billed {
                    return Err(AppError::OrderNotEmpty);
                }
            }
            order_id
        };

        let mut envelopes = Vec::with_capacity(2);
        let mut messages = Vec::with_capacity(2);
        if let Some(order_id) = order_id {
            let (envelope, message) = self.prepare(&ctx, &SalesOrderClosed { order_id })?;
            envelopes.push(envelope);
            messages.push(message);
        }
        let (envelope, message) = self.prepare(&ctx, &SalesTableClosed { table_id })?;
        envelopes.push(envelope);
        messages.push(message);
        self.append_and_publish(envelopes, messages).await?;
        {
            let mut projection = self.lock_projection();
            if let Some(order_id) = order_id {
                projection.close_order(order_id);
            }
            projection.set_table(table_id, decision.next_state);
        }
        // A voided line's note is gone already; a released order takes whatever else it held.
        self.forget_notes_off_screen();
        Ok(TableView {
            table_id,
            state: decision.next_state,
        })
    }

    /// Moves the guests at a table, and the order they are eating from, to a free table
    /// (`sales.table.transferred`).
    ///
    /// Two floor moves, both the table machine's, and one event. The table they left waits to be
    /// cleared, as it does when guests pay and go; the table they moved to is seated with the order
    /// they already had: its lines, its tickets in the kitchen, and the time they sat down. Nothing
    /// is rung again and nothing is fired again. A ticket names its order, and every screen reads
    /// the order's table from where the order is now.
    ///
    /// Only before the bill. A table whose guests have asked for it pays where it sits, which the
    /// machine enforces by allowing the move from `Occupied` alone. And not while the table's order
    /// is a guest's submission still waiting for staff (ADR-0116): it took the table over from any
    /// order a server had started there, and refusing it hands the table back to that order, which
    /// only works while both are at the table they opened on. Staff decide on it first.
    ///
    /// [`Permission::TransferOrder`](pos_core::permission::Permission::TransferOrder), which a server holds by default and which asks for no PIN.
    /// It mints no order, so a replaced machine still moves guests between the tables it is
    /// draining (ADR-0123).
    ///
    /// # Errors
    ///
    /// [`AppError::Domain`] if the actor may not move orders, the guests' table is not occupied, or
    /// the table they move to is not free (their own included); [`AppError::NoOpenOrder`] if the
    /// table holds no order; [`AppError::AwaitingStaffConfirmation`] as above; or [`AppError`] if
    /// the store cannot be written.
    pub async fn transfer_table(
        &self,
        actor: Actor,
        from_table_id: TableId,
        to_table_id: TableId,
    ) -> Result<TransferView, AppError> {
        let ctx = self.decision_ctx(actor)?;
        let session = self.session();
        let (from_state, to_state, order_id, held) = {
            let projection = self.lock_projection();
            let order_id = projection.order_for_table(from_table_id);
            (
                projection.table_state(from_table_id),
                projection.table_state(to_table_id),
                order_id,
                order_id.is_some_and(|order_id| {
                    projection.awaits_staff_confirmation(order_id, &session)
                }),
            )
        };
        let left = decide_table(from_state, TableCommand::Transfer, &ctx)?;
        let seated = decide_table(to_state, TableCommand::Seat, &ctx)?;
        let order_id = order_id.ok_or(AppError::NoOpenOrder)?;
        if held {
            return Err(AppError::AwaitingStaffConfirmation);
        }

        let payload = SalesTableTransferred {
            order_id,
            from_table_id,
            to_table_id,
        };
        self.commit_and_publish(&ctx, &payload).await?;
        {
            let mut projection = self.lock_projection();
            projection.set_table(from_table_id, left.next_state);
            projection.set_table(to_table_id, seated.next_state);
            projection.move_order(order_id, from_table_id, to_table_id);
        }
        Ok(TransferView {
            order_id,
            from: TableView {
                table_id: from_table_id,
                state: left.next_state,
            },
            to: TableView {
                table_id: to_table_id,
                state: seated.next_state,
            },
        })
    }

    /// The order a table is running now, for a caller that has to name it — the pre-bill prints the
    /// same short reference its kitchen tickets carry.
    #[must_use]
    pub fn order_for_table(&self, table_id: TableId) -> Option<OrderId> {
        self.lock_projection().order_for_table(table_id)
    }
}

#[cfg(test)]
mod tests {
    use pos_proto::TableState;
    use pos_proto::ids::TableId;
    use pos_proto::ulid::Ulid;

    use crate::app::tests::{actor, edge};

    #[test]
    fn seating_a_table_opens_it() {
        pos_fakes::executor::run_ready(async {
            let edge = edge();
            let table = TableId::new(Ulid::from_u128(100));

            assert_eq!(edge.table_state(table), TableState::Free);
            let view = edge.seat_table(actor(), table, None).await.expect("seats");
            assert_eq!(view.state, TableState::Occupied);
            assert_eq!(edge.table_state(table), TableState::Occupied);
        });
    }

    #[test]
    fn a_second_device_sees_the_change_over_the_fanout() {
        pos_fakes::executor::run_ready(async {
            let edge = edge();
            let table = TableId::new(Ulid::from_u128(101));
            let mut device_a = edge.fanout().subscribe();
            let mut device_b = edge.fanout().subscribe();

            edge.seat_table(actor(), table, None).await.expect("seats");

            for device in [&mut device_a, &mut device_b] {
                let frame = device.try_recv().expect("a frame reached the device");
                assert!(frame.contains("sales.table.opened"));
            }
        });
    }

    #[test]
    fn an_illegal_transition_is_refused_and_nothing_is_published() {
        pos_fakes::executor::run_ready(async {
            let edge = edge();
            let table = TableId::new(Ulid::from_u128(102));
            let mut device = edge.fanout().subscribe();

            // Cleaning a Free table is not a legal move (clean is needs-cleaning -> free).
            let refused = edge.clean_table(actor(), table).await;
            assert!(refused.is_err(), "an illegal transition is refused");
            assert!(
                device.try_recv().is_err(),
                "a refused command publishes nothing"
            );
            assert_eq!(
                edge.table_state(table),
                TableState::Free,
                "state is unchanged"
            );
        });
    }
}
