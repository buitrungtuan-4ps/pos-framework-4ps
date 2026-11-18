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
use pos_proto::ids::{BillId, DeviceId, ReasonCodeId};
use pos_proto::money::Money;
use pos_proto::reason_codes::ReasonAction;

use crate::app::{AppError, Approval, Edge, TillScope};

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
        let voided = self
            .void_bill_at(
                actor,
                TillScope::default(),
                bill_id,
                reason_code_id,
                approval,
            )
            .await?;
        Ok(voided.state)
    }

    /// [`Self::void_bill`] at the till `scope` says the device is: a bill that holds payments
    /// taken one at a time gives each back
    /// ([ADR-0220](../../../../../docs/adr/0220-a-bill-takes-its-payments-one-at-a-time.md)
    /// decision 4), as a void after payment does (ADR-0184 decision 6): its cash out of this till's
    /// drawer, stamped with that drawer's shift, and every other tender recorded as reversed.
    ///
    /// # Errors
    ///
    /// As [`Self::void_bill`], and for a bill holding cash the drawer refusals of a void after
    /// payment.
    pub async fn void_bill_at(
        &self,
        actor: Actor,
        scope: TillScope,
        bill_id: BillId,
        reason_code_id: ReasonCodeId,
        approval: Option<&Approval>,
    ) -> Result<VoidedBill, AppError> {
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

        let paid = self.lock_projection().paid(bill_id);
        let (drawer, shift, overrider) = match paid {
            Some(paid) if !paid.cash.is_zero() => {
                self.cash_drawer_for(&mut ctx, scope, approval)?
            }
            Some(_) => (None, self.drawer_shift(scope), None),
            None => (None, None, None),
        };
        let voided = BillingBillVoided {
            bill_id,
            reason_code_id,
        };
        let (voided_envelope, voided_message) = self.prepare_on(&ctx, &voided, shift)?;
        let mut envelopes = vec![voided_envelope];
        let mut messages = vec![voided_message];
        for (permission, approved_by) in [
            (Permission::VoidBill, approver),
            (Permission::OverrideDrawerAssignment, overrider),
        ] {
            if let Some(approved_by) = approved_by {
                let record = Self::override_record(permission, approved_by, None);
                let (envelope, message) = self.prepare_on(&ctx, &record, shift)?;
                envelopes.push(envelope);
                messages.push(message);
            }
        }
        self.append_and_publish(envelopes, messages).await?;

        let mut projection = self.lock_projection();
        if paid.is_some() {
            projection
                .void_settled_record(bill_id, shift)
                .map_err(AppError::Domain)?;
        } else {
            projection.void_bill_record(bill_id);
        }
        Ok(VoidedBill {
            state: next_state,
            cash_returned: paid.map(|paid| paid.cash),
            other_reversed: paid.map(|paid| paid.other),
            drawer_till: drawer,
        })
    }
}

/// What voiding a bill before it settled came to, and what it gave back where payments had been
/// taken towards it (ADR-0220 decision 4).
#[derive(Debug, Clone, Copy)]
pub struct VoidedBill {
    /// Its state now, `VOIDED`.
    pub state: BillState,
    /// The cash given back from this till's drawer, where payments were taken towards it.
    pub cash_returned: Option<Money>,
    /// Every other tender recorded as reversed, where payments were taken towards it.
    pub other_reversed: Option<Money>,
    /// The till whose drawer the cash leaves, where the store keeps one per till.
    pub drawer_till: Option<DeviceId>,
}

impl VoidedBill {
    /// Whether to open the drawer to hand cash back: only where there is cash.
    #[must_use]
    pub fn open_drawer(&self) -> bool {
        self.cash_returned.is_some_and(|cash| !cash.is_zero())
    }
}
