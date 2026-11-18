// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! Closing a tableless order nothing was sold on: a bar tab opened by mistake, or whose guest left
//! before ordering
//! ([ADR-0221](../../../../../docs/adr/0221-a-bar-tab-is-an-order-without-a-table-that-stays-open.md)).
//!
//! The counter's [`Edge::release_table`]: until now such an order could end only by a bill on
//! nothing, which can never settle, so an empty tab stayed on every till's list for good.

use pos_core::decision::Actor;
use pos_core::permission::Permission;
use pos_ports::event_store::EventStore;
use pos_proto::BillState;
use pos_proto::events::SalesOrderClosed;
use pos_proto::ids::{OrderId, ReasonCodeId};
use pos_proto::reason_codes::ReasonAction;

use crate::app::{AppError, Edge};

impl<S: EventStore> Edge<S> {
    /// Closes an open tableless order with nothing sold on it (`sales.order.closed`, with the
    /// reason given), which drops it from the counter's and the tabs' lists.
    ///
    /// The reason comes from the store's list for `REASON_ACTION_CLOSE_ORDER`, and is owed only
    /// where that list gives one.
    ///
    /// # Errors
    ///
    /// [`AppError::Superseded`] on a replaced box, [`AppError::Domain`] if the actor lacks
    /// `sales.line.add`, [`AppError::UnknownOrder`] for an order that is not an open tableless one,
    /// [`AppError::OrderNotEmpty`] while a line is not voided or a bill is open,
    /// [`AppError::CloseReasonNotValid`] for a reason the list does not give for closing, or none
    /// while it gives one, or [`AppError`] if the store cannot be written.
    pub async fn close_empty_order(
        &self,
        actor: Actor,
        order_id: OrderId,
        reason_code_id: Option<ReasonCodeId>,
    ) -> Result<(), AppError> {
        self.refuse_if_superseded()?;
        let ctx = self.decision_ctx(actor)?;
        // Ending an order at the counter is the counterpart of starting one (ADR-0158).
        ctx.require(Permission::AddLine)?;
        let reasons = &self.session().reason_codes;
        let valid = match reason_code_id {
            Some(reason) => reasons.accepts(reason, ReasonAction::CloseOrder),
            None => reasons
                .for_action(ReasonAction::CloseOrder)
                .next()
                .is_none(),
        };
        if !valid {
            return Err(AppError::CloseReasonNotValid);
        }
        {
            let projection = self.lock_projection();
            if !projection.open_tableless(order_id) {
                return Err(AppError::UnknownOrder);
            }
            let sold = !projection.sold_line_ids(order_id).is_empty();
            let billed = projection
                .latest_bill_in(order_id, BillState::Open)
                .is_some();
            if sold || billed {
                return Err(AppError::OrderNotEmpty);
            }
        }
        let closed = SalesOrderClosed {
            order_id,
            reason_code_id,
        };
        self.commit_and_publish(&ctx, &closed).await?;
        self.lock_projection().close_order(order_id);
        // A voided line's note is gone already; a closed order takes whatever else it held.
        self.forget_notes_off_screen();
        Ok(())
    }
}
