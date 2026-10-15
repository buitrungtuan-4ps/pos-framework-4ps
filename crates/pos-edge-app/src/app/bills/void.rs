// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! Voiding a whole bill before it settles, citing a reason from the store's managed list.

use pos_core::StateMachine;
use pos_core::decision::Actor;
use pos_core::error::DomainError;
use pos_core::machines::{Bill, BillTrigger};
use pos_core::permission::Permission;
use pos_ports::event_store::EventStore;
use pos_proto::BillState;
use pos_proto::events::BillingBillVoided;
use pos_proto::ids::{BillId, ReasonCodeId};
use pos_proto::reason_codes::ReasonAction;

use crate::app::{AppError, Approval, Edge};

impl<S: EventStore> Edge<S> {
    /// Voids a whole bill before it settles, citing a reason from the store's managed list
    /// (`docs/pos-spec.md` §6, §11 item 2; ADR-0115, roadmap B2.2).
    ///
    /// [`Permission::VoidBill`] is `pin: true` and, unlike a line, has no unfired shape — a bill is
    /// money either way — so a manager's PIN is required for every one. A settled bill is not
    /// voidable: `pos_core`'s bill machine makes `Settled` terminal on purpose
    /// (`crates/pos-core/src/machines.rs`), and the way to reverse a settled bill is a refund, which
    /// is its own permission and its own reason.
    ///
    /// # Errors
    ///
    /// [`AppError::UnknownBill`] for a bill the edge does not know;
    /// [`AppError::VoidReasonNotValid`] if the reason is not one the store holds for voiding a bill;
    /// [`AppError::ApprovalRequired`] or [`AppError::ApprovalRefused`] if the manager PIN is missing
    /// or does not authorise; [`AppError::Domain`] if the bill has already settled or been voided;
    /// [`AppError::Port`] if the store cannot be written.
    pub async fn void_bill(
        &self,
        actor: Actor,
        bill_id: BillId,
        reason_code_id: ReasonCodeId,
        approval: Option<&Approval>,
    ) -> Result<BillState, AppError> {
        let mut ctx = self.decision_ctx(actor)?;
        let session = self.session();
        let record = self
            .lock_projection()
            .bill(bill_id)
            .ok_or(AppError::UnknownBill)?;
        if !session
            .reason_codes
            .accepts(reason_code_id, ReasonAction::VoidBill)
        {
            return Err(AppError::VoidReasonNotValid);
        }
        let approver = self.resolve_step_up(&mut ctx, Permission::VoidBill, approval)?;
        ctx.require(Permission::VoidBill)?;
        // The machine owns whether this bill may be voided at all: `Settled` and `Voided` are both
        // terminal, so a second void and a void-after-payment are refused here rather than by a
        // hand-rolled state check that could disagree with the machine.
        let next_state = Bill::step(record.state, BillTrigger::Void)
            .map_err(DomainError::from)
            .map_err(AppError::Domain)?;

        let voided = BillingBillVoided {
            bill_id,
            reason_code_id,
        };
        let (voided_envelope, voided_message) = self.prepare(&ctx, &voided)?;
        let mut envelopes = vec![voided_envelope];
        let mut messages = vec![voided_message];
        if let Some(approver) = approver {
            let override_record = Self::override_record(Permission::VoidBill, approver, None);
            let (envelope, message) = self.prepare(&ctx, &override_record)?;
            envelopes.push(envelope);
            messages.push(message);
        }
        self.append_and_publish(envelopes, messages).await?;

        self.lock_projection().void_bill_record(bill_id);
        Ok(next_state)
    }
}
