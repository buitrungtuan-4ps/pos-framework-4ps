// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! Voiding a bill that was paid, whole
//! ([ADR-0184](../../../../../docs/adr/0184-a-settled-bill-is-voided-whole-not-refunded.md)).

use pos_core::decision::{Actor, BillCommand, DecisionCtx, Effect, SettledVoid, decide_bill};
use pos_core::permission::Permission;
use pos_ports::event_store::EventStore;
use pos_proto::ShiftState;
use pos_proto::events::BillingBillVoided;
use pos_proto::ids::{BillId, ReasonCodeId};
use pos_proto::reason_codes::ReasonAction;

use crate::app::{
    AppError, Approval, BillRecord, Edge, PaidBill, SettledReceipt, SettledVoidView, TillScope,
};

/// What a void after payment reads of the bill before it decides: the bill, its receipt and what
/// its tenders put on it.
struct PaidRecord {
    bill: BillRecord,
    receipt: Option<SettledReceipt>,
    paid: PaidBill,
    taking_shift_open: bool,
}

impl<S: EventStore> Edge<S> {
    /// Voids a bill that was paid, whole, citing a reason from the store's `VOID_BILL` list
    /// (ADR-0184 decisions 2–8).
    ///
    /// What no approval changes is decided first — the bill must be settled, inside the store's
    /// `billing.settled_void_window`, and carry no issued e-invoice — so nobody is asked for a PIN
    /// for a void that would be refused anyway. Then `billing.bill.void_settled`, held directly or
    /// approved by a holder's code and PIN. The bill's cash leaves the drawer of the till the void is
    /// done at (ADR-0167), refused as cash into that drawer would be; every other tender is
    /// recorded as reversed; tips are untouched. `billing.bill.voided` is stamped with that drawer's
    /// shift and committed with the approval's override in one transaction, so a replay gives the
    /// same cash back from the same drawer.
    ///
    /// No store issues e-invoices yet (ADR-0156 is proposed, not built), so no bill carries one and
    /// the edge says so (ADR-0184, amendment of 2026-11-02).
    ///
    /// # Errors
    ///
    /// [`AppError::UnknownBill`]; [`AppError::VoidReasonNotValid`] for a reason the store does not
    /// hold for voiding a bill; [`AppError::Domain`] with a transition refusal for a bill that is
    /// not settled, `SettledVoidWindowClosed`, or the permission's refusal;
    /// [`AppError::ApprovalRequired`], [`AppError::ApprovalRefused`] or
    /// [`AppError::ApproverLockedOut`] from the step-up; the drawer refusals of a settle's cash
    /// where the bill took cash; [`AppError::Port`] if the store cannot be written.
    pub async fn void_settled_bill(
        &self,
        actor: Actor,
        scope: TillScope,
        bill_id: BillId,
        reason_code_id: ReasonCodeId,
        approval: Option<&Approval>,
    ) -> Result<SettledVoidView, AppError> {
        let mut ctx = self.decision_ctx(actor)?;
        let session = self.session();
        let record = self.paid_record(bill_id, &ctx)?;
        if !session
            .reason_codes
            .accepts(reason_code_id, ReasonAction::VoidBill)
        {
            return Err(AppError::VoidReasonNotValid);
        }
        let facts = SettledVoid {
            pin_verified: false,
            window: session.billing.settled_void_window(),
            bill_business_date: record
                .receipt
                .as_ref()
                .map_or(ctx.business_date, |receipt| receipt.business_date),
            taking_shift_open: record.taking_shift_open,
            e_invoice_issued: false,
        };
        // The facts, decided as if the act were held, before anybody is asked to approve it.
        let mut probe = ctx.clone();
        probe.granted = probe.granted.with(Permission::VoidSettledBill);
        probe.granted_directly = probe.granted_directly.with(Permission::VoidSettledBill);
        let _facts_hold = decide_bill(record.bill.state, BillCommand::VoidSettled(facts), &probe)?;

        let approver = self.resolve_step_up(&mut ctx, Permission::VoidSettledBill, approval)?;
        let cash = record.paid.cash;
        let (drawer, shift, overrider) = if cash.is_zero() {
            (None, self.drawer_shift(scope), None)
        } else {
            self.cash_drawer_for(&mut ctx, scope, approval)?
        };
        let decision = decide_bill(
            record.bill.state,
            BillCommand::VoidSettled(SettledVoid {
                pin_verified: approver.is_some(),
                ..facts
            }),
            &ctx,
        )?;

        let voided = BillingBillVoided {
            bill_id,
            reason_code_id,
        };
        let (envelope, message) = self.prepare_on(&ctx, &voided, shift)?;
        let job_id = envelope.event_id;
        let mut envelopes = vec![envelope];
        let mut messages = vec![message];
        for (permission, approved_by) in [
            (Permission::VoidSettledBill, approver),
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
        self.lock_projection()
            .void_settled_record(bill_id, shift)
            .map_err(AppError::Domain)?;

        Ok(SettledVoidView {
            bill_id,
            state: decision.next_state,
            receipt_number: record
                .receipt
                .as_ref()
                .map_or(0, |receipt| receipt.receipt_number),
            total_due: record
                .receipt
                .as_ref()
                .map_or(record.paid.cash, |receipt| receipt.total_due),
            cash_returned: cash,
            other_reversed: record.paid.other,
            open_drawer: !cash.is_zero(),
            drawer_till: drawer,
            print_void_slip: decision.effects.contains(&Effect::PrintVoidBillSlip),
            job_id,
            voided_time: ctx.now,
            reason: session.reason_codes.find(reason_code_id).cloned(),
            till_name: self.till_name(scope.own),
        })
    }

    /// The bill, its receipt, what it was paid with and whether the shift its settle was stamped
    /// with is still open, read under one lock.
    fn paid_record(&self, bill_id: BillId, ctx: &DecisionCtx) -> Result<PaidRecord, AppError> {
        let projection = self.lock_projection();
        let bill = projection.bill(bill_id).ok_or(AppError::UnknownBill)?;
        let paid = projection
            .paid(bill_id)
            .unwrap_or_else(|| PaidBill::nothing(ctx.currency));
        let taking_shift_open = paid
            .shift_id
            .and_then(|shift_id| projection.shift(shift_id))
            .is_some_and(|shift| shift.state == ShiftState::Open);
        Ok(PaidRecord {
            receipt: projection.settled.get(&bill_id).cloned(),
            bill,
            paid,
            taking_shift_open,
        })
    }
}

#[cfg(test)]
mod tests;
