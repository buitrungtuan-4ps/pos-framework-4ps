// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! An inbound order that waits for a member of staff: the waiting set, what the waiting screen
//! shows, and confirming or rejecting it.

use pos_core::decision::Actor;
use pos_core::permission::Permission;
use pos_ports::event_store::EventStore;
use pos_proto::OrderLineState;
use pos_proto::events::{
    SalesOrderClosed, SalesOrderConfirmedByStaff, SalesOrderRejectedByStaff, SalesTableMerged,
};
use pos_proto::ids::{OrderId, OrderLineId, ReasonCodeId, TableId};
use pos_proto::kitchen_release::KitchenRelease;
use pos_proto::qr::TableOrder;
use pos_proto::reason_codes::ReasonAction;

use crate::app::{AppError, Edge, LineView, StaffDecision, WaitingLine};

impl<S: EventStore> Edge<S> {
    /// Every guest order still waiting for a member of staff, oldest first
    /// ([ADR-0116](../../../../../docs/adr/0116-the-qr-hold-is-derived-and-it-gates-firing.md)).
    ///
    /// Derived from the log and the live `qr` policy, so a store that turns the hold off sees this
    /// list empty on the next read rather than having to release each order by hand.
    #[must_use]
    pub fn orders_awaiting_staff_confirmation(&self) -> Vec<OrderId> {
        let session = self.session();
        self.lock_projection()
            .orders_awaiting_staff_confirmation(&session)
    }

    /// Whether one order is still waiting for a member of staff (ADR-0116).
    #[must_use]
    pub fn awaits_staff_confirmation(&self, order_id: OrderId) -> bool {
        let session = self.session();
        self.lock_projection()
            .awaits_staff_confirmation(order_id, &session)
    }

    /// The line ids on one order, oldest first.
    ///
    /// A ULID sorts by mint time, so this is the order the lines were added in.
    #[must_use]
    pub fn order_line_ids(&self, order_id: OrderId) -> Vec<OrderLineId> {
        self.lock_projection().line_ids_for_order(order_id).to_vec()
    }

    /// The lines and table of one waiting order, for the confirmation screen (ADR-0116).
    ///
    /// Returns the display name rather than the menu item id: the line captured the name the guest
    /// saw at the moment they ordered (§14.2), and re-reading the live menu could show a member of
    /// staff a name the guest never saw.
    #[must_use]
    pub fn waiting_order_view(&self, order_id: OrderId) -> (Vec<WaitingLine>, Option<TableId>) {
        let session = self.session();
        let projection = self.lock_projection();
        let lines = projection
            .lines_for_order(order_id)
            .into_iter()
            .map(|record| WaitingLine {
                // The same fallback the counter list uses: an item the store has since removed
                // from its menu must still be *shown*, or a member of staff would be asked to
                // agree to an order with a line missing from it.
                display_name: session.menu_entry(record.menu_item_id).map_or_else(
                    || record.menu_item_id.to_string(),
                    |entry| entry.display_name.as_str().to_owned(),
                ),
                quantity: record.quantity,
                line_total: record.line_total,
            })
            .collect();
        let table_id = projection
            .order_origin(order_id)
            .and_then(|origin| origin.table_id);
        (lines, table_id)
    }

    /// Releases a guest's order to the kitchen (`sales.order.confirmed_by_staff`), ADR-0116.
    ///
    /// The order's lines become fireable. Under `KITCHEN_RELEASE_CONFIRM_FIRES` they are also fired
    /// in the same transaction, and the lines fired are returned for their tickets (ADR-0247);
    /// otherwise nothing is fired and the list is empty. Idempotent in the sense
    /// that matters operationally: a second confirmation is refused with
    /// [`AppError::NotAwaitingStaffConfirmation`] rather than writing a second event, so two
    /// devices racing on the same order produce one decision and one audit entry.
    ///
    /// Where the store sets `qr.table_order` to `TABLE_ORDER_JOIN` (ADR-0160 item 2) and the guest's
    /// table has an order that can take a line, the confirmed order joins it in the same
    /// transaction (`sales.table.merged`): its lines are that order's from then on, and the table
    /// holds that order again. An order with a bill of its own, or at a table with none to join,
    /// is only confirmed, as before.
    ///
    /// # Errors
    ///
    /// [`AppError::NotAwaitingStaffConfirmation`] if the order is not a held guest submission or
    /// staff have already decided, [`AppError::Domain`] if the actor lacks
    /// `Permission::ConfirmQrOrder`, or [`AppError`] if the store cannot be written.
    pub async fn confirm_inbound_order(
        &self,
        actor: Actor,
        order_id: OrderId,
    ) -> Result<Vec<LineView>, AppError> {
        let ctx = self.decision_ctx(actor)?;
        ctx.require(Permission::ConfirmQrOrder)?;
        if !self.awaits_staff_confirmation(order_id) {
            return Err(AppError::NotAwaitingStaffConfirmation);
        }
        // One tap accepts and sends where the store says so (ADR-0247): the order's unsent lines
        // fire in the confirmation's own transaction, decided and guarded as any send is, under
        // the person who confirms. Before a join, which moves them to the table's order.
        let fires_too = self.confirm_fires(order_id);
        if fires_too {
            self.check_flow_guard("sales.order.fire", order_id)?;
        }

        // The table's order the guest's lines join, read before the write as a waiter's add-line
        // reads the bill it must not land after.
        let session = self.session();
        let joins = if session.qr.table_order() == TableOrder::Join {
            let projection = self.lock_projection();
            projection
                .order_origin(order_id)
                .and_then(|origin| origin.table_id)
                .filter(|_| projection.bill_for_order(order_id).is_none())
                .and_then(|table_id| {
                    projection.joinable_order_at(table_id, Some(order_id), &session)
                })
        } else {
            None
        };

        let confirmed = SalesOrderConfirmedByStaff { order_id };
        let (envelope, message) = self.prepare(&ctx, &confirmed)?;
        let mut envelopes = vec![envelope];
        let mut messages = vec![message];
        // Prepared after the confirmation, so the log reads confirmed, then sent.
        let fires = if fires_too {
            let unfired = self.lock_projection().unfired_lines_for_order(order_id);
            Some(self.prepare_fires(&ctx, unfired, |_| None, None)?)
        } else {
            None
        };
        let applied = fires.map(|(fired, announced, applied)| {
            envelopes.extend(fired);
            messages.extend(announced);
            applied
        });
        if let Some(target_order_id) = joins {
            let merged = SalesTableMerged {
                target_order_id,
                merged_order_id: order_id,
            };
            let (envelope, message) = self.prepare(&ctx, &merged)?;
            envelopes.push(envelope);
            messages.push(message);
        }
        self.append_and_publish(envelopes, messages).await?;
        {
            let mut projection = self.lock_projection();
            projection.record_staff_decision(order_id, StaffDecision::Confirmed);
            if let Some(target_order_id) = joins {
                projection.merge_order(target_order_id, order_id);
            }
        }
        Ok(applied.map_or_else(Vec::new, |applied| self.apply_fires(&ctx, applied)))
    }

    /// Whether confirming `order_id` also sends it: its channel's or its vendor's kitchen release is
    /// `KITCHEN_RELEASE_CONFIRM_FIRES` (ADR-0247 decision 1).
    #[must_use]
    pub fn confirm_fires(&self, order_id: OrderId) -> bool {
        let session = self.session();
        let projection = self.lock_projection();
        let channel = projection
            .order_origin(order_id)
            .and_then(|origin| origin.channel);
        session.kitchen_release(channel, projection.vendor(order_id), order_id)
            == KitchenRelease::ConfirmFires
    }

    /// Refuses a guest's order (`sales.order.rejected_by_staff`, then `sales.order.closed`),
    /// ADR-0116.
    ///
    /// The reason is validated against the store's managed list before the event is written
    /// ([ADR-0115](../../../../../docs/adr/0115-reason-codes-are-a-managed-list.md)), for the
    /// `REASON_ACTION_REJECT_ORDER` action — which is what makes the event's own doc comment
    /// (*"the mandatory reason, from the cloud-managed list"*) true rather than aspirational.
    ///
    /// A rejection closes the order, because a refused guest order is not a live order waiting on
    /// the floor: left open it would appear on the counter list and on the table's bill. An order
    /// with a line already fired is refused instead — that is a void, with a void's permission and
    /// its own reason (roadmap B2.2).
    ///
    /// # Errors
    ///
    /// [`AppError::NotAwaitingStaffConfirmation`] if the order is not awaiting a decision,
    /// [`AppError::ReasonCodeNotValid`] if the reason is not valid for rejecting an order,
    /// [`AppError::AlreadyFired`] if the kitchen has already started, [`AppError::Domain`] if the
    /// actor lacks `Permission::ConfirmQrOrder`, or [`AppError`] if the store cannot be written.
    pub async fn reject_inbound_order(
        &self,
        actor: Actor,
        order_id: OrderId,
        reason_code_id: ReasonCodeId,
    ) -> Result<(), AppError> {
        let ctx = self.decision_ctx(actor)?;
        ctx.require(Permission::ConfirmQrOrder)?;
        if !self.awaits_staff_confirmation(order_id) {
            return Err(AppError::NotAwaitingStaffConfirmation);
        }
        if !self
            .session()
            .reason_codes
            .accepts(reason_code_id, ReasonAction::RejectOrder)
        {
            return Err(AppError::ReasonCodeNotValid);
        }
        // Checked before the write rather than after: a fired line has already consumed stock and
        // printed a ticket, and "rejected" would tell the log a story the kitchen could contradict.
        if self
            .lock_projection()
            .lines_for_order(order_id)
            .iter()
            .any(|line| line.state == OrderLineState::Fired)
        {
            return Err(AppError::AlreadyFired);
        }

        let rejected = SalesOrderRejectedByStaff {
            order_id,
            reason_code_id,
        };
        let closed = SalesOrderClosed {
            order_id,
            reason_code_id: None,
        };
        let (rejected_envelope, rejected_message) = self.prepare(&ctx, &rejected)?;
        let (closed_envelope, closed_message) = self.prepare(&ctx, &closed)?;
        self.append_and_publish(
            vec![rejected_envelope, closed_envelope],
            vec![rejected_message, closed_message],
        )
        .await?;
        {
            let mut projection = self.lock_projection();
            projection.record_staff_decision(order_id, StaffDecision::Rejected);
            // The order ends here as the replay of its `sales.order.closed` ends it, so a live box
            // and a restarted one hold the same thing.
            projection.close_order(order_id);
        }
        self.forget_notes_off_screen();
        Ok(())
    }
}
