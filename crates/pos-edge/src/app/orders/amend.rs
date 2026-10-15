// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! Changing a line once it is taken: setting its quantity, and voiding it with a reason.

use pos_core::decision::{Actor, LineCommand, decide_line};
use pos_core::error::DomainError;
use pos_core::permission::Permission;
use pos_ports::event_store::EventStore;
use pos_proto::OrderLineState;
use pos_proto::events::{SalesOrderLineUpdated, SalesOrderLineVoided};
use pos_proto::ids::{OrderLineId, ReasonCodeId};
use pos_proto::money::Rounding;
use pos_proto::quantity::Quantity;
use pos_proto::reason_codes::ReasonAction;

use crate::app::{AppError, Approval, Edge, LineView};

impl<S: EventStore> Edge<S> {
    /// Voids one order line, citing a reason from the store's managed list (`docs/pos-spec.md` §5,
    /// §11 item 2; [ADR-0115](../../../docs/adr/0115-reason-codes-are-a-managed-list.md), roadmap
    /// B2.2).
    ///
    /// # The two shapes of a void
    ///
    /// A line that never fired is an ordinary cancel: nothing was made, no stock moved, and any
    /// member of staff who can take the order can take it back off. A line that **has** fired is the
    /// fraud-sensitive one — the kitchen made it, the stock is gone, and the money is what changes —
    /// so it needs [`Permission::VoidFiredLine`], and because that permission is `pin: true` it
    /// needs a manager's PIN with it. `pos_core::decide_line` has enforced exactly that since P3;
    /// what did not exist until now was a caller, so the check had never run in production.
    ///
    /// The reason is validated against the **synced** list, which is what makes both events' "from
    /// the cloud-managed list" true rather than aspirational.
    ///
    /// # Errors
    ///
    /// [`AppError::UnknownLine`] for a line the edge does not know;
    /// [`AppError::VoidReasonNotValid`] if the reason is not one the store holds for voiding a line;
    /// [`AppError::ApprovalRequired`] or [`AppError::ApprovalRefused`] if a fired line's manager PIN
    /// is missing or does not authorise; [`AppError::Domain`] if the line cannot leave its state or
    /// the actor is not permitted; [`AppError::Port`] if the store cannot be written.
    pub async fn void_line(
        &self,
        actor: Actor,
        order_line_id: OrderLineId,
        reason_code_id: ReasonCodeId,
        approval: Option<&Approval>,
    ) -> Result<LineView, AppError> {
        let mut ctx = self.decision_ctx(actor)?;
        let session = self.session();
        let record = self
            .lock_projection()
            .line(order_line_id)
            .ok_or(AppError::UnknownLine)?;
        if !session
            .reason_codes
            .accepts(reason_code_id, ReasonAction::VoidLine)
        {
            return Err(AppError::VoidReasonNotValid);
        }

        // Only a fired line needs the step-up, because only a fired line costs the store something.
        let was_fired = record.state == OrderLineState::Fired;
        let approver = if was_fired {
            self.resolve_step_up(&mut ctx, Permission::VoidFiredLine, approval)?
        } else {
            None
        };

        let decision = decide_line(
            record.state,
            LineCommand::Void {
                pin_verified: approver.is_some(),
            },
            &ctx,
            &session.recipes,
        )?;

        let voided = SalesOrderLineVoided {
            order_id: record.order_id,
            order_line_id,
            reason_code_id,
            was_fired,
        };
        let (voided_envelope, voided_message) = self.prepare(&ctx, &voided)?;
        let mut envelopes = vec![voided_envelope];
        let mut messages = vec![voided_message];
        // One transaction for the void and the override that authorised it: a log holding the void
        // without the approval would show an act nobody could have performed.
        if let Some(approver) = approver {
            let override_record = Self::override_record(Permission::VoidFiredLine, approver, None);
            let (envelope, message) = self.prepare(&ctx, &override_record)?;
            envelopes.push(envelope);
            messages.push(message);
        }
        self.append_and_publish(envelopes, messages).await?;

        self.lock_projection()
            .set_line_state(order_line_id, decision.next_state);
        self.forget_notes_off_screen();
        Ok(LineView {
            order_id: record.order_id,
            order_line_id,
            state: decision.next_state,
            fired: None,
        })
    }

    /// Changes how many of a line the order carries, while it is still editable.
    ///
    /// "Three beers" was three taps and three lines, because the till could only ever add one of a
    /// thing (`quantity: { milli: 1000 }`, hard-coded at the call site with a comment saying so).
    /// This is the command behind the stepper that replaces them, and it emits
    /// `sales.order_line.updated` — an event `pos-proto` has carried, with exactly this shape,
    /// since the schema was written, and which nothing has ever emitted.
    ///
    /// **The new total is the captured unit price times the new quantity, computed here.** The caller
    /// sends a quantity and nothing else. That is not the edge inventing a price: `unit_price` is
    /// the figure the device captured when it added the line and `sales.order_line.added` carried,
    /// so multiplying it is exactly what the device would do — with the difference that the guest
    /// cannot be charged an extended total that does not follow from the price they were quoted.
    /// Today's menu is not consulted, because a line never re-reads the live menu (§14.2) and that
    /// holds when it changes as much as when it is added.
    ///
    /// # Errors
    ///
    /// [`AppError::UnknownLine`] for a line this edge does not hold, [`AppError::Domain`] when the
    /// line has already gone to the kitchen — a fired line is refused by the state machine, not by a
    /// permission, because the food exists and stock moved against the old number, so that case is a
    /// void and a fresh line — and [`AppError::Domain`] again if the multiplication overflows.
    pub async fn set_line_quantity(
        &self,
        actor: Actor,
        order_line_id: OrderLineId,
        quantity: Quantity,
    ) -> Result<LineView, AppError> {
        let ctx = self.decision_ctx(actor)?;
        let session = self.session();
        let record = self
            .lock_projection()
            .line(order_line_id)
            .ok_or(AppError::UnknownLine)?;

        let decision = decide_line(
            record.state,
            LineCommand::SetQuantity { quantity },
            &ctx,
            &session.recipes,
        )?;

        // Integer arithmetic on the captured price, rounded the way every other money path here
        // rounds (§1 rule 10: money is never a float).
        let line_total = record
            .unit_price
            .mul_quantity(quantity, Rounding::HalfUp)
            .map_err(|error| AppError::Domain(DomainError::Money(error)))?;

        let payload = SalesOrderLineUpdated {
            order_id: record.order_id,
            order_line_id,
            quantity,
            line_total,
        };
        self.commit_and_publish(&ctx, &payload).await?;

        self.lock_projection()
            .set_line_quantity(order_line_id, quantity, line_total);

        Ok(LineView {
            order_id: record.order_id,
            order_line_id,
            state: decision.next_state,
            // Nothing was sent anywhere: an amend is only legal before the kitchen is told.
            fired: None,
        })
    }
}
