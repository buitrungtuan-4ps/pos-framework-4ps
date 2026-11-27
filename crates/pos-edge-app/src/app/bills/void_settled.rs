// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! Voiding a bill that was paid, whole
//! ([ADR-0184](../../../../../docs/adr/0184-a-settled-bill-is-voided-whole-not-refunded.md)).

use pos_core::decision::{
    Actor, BillCommand, DecisionCtx, Effect, InvoicedVoid, SettledVoid, decide_bill,
};
use pos_core::permission::Permission;
use pos_ports::event_store::EventStore;
use pos_proto::ShiftState;
use pos_proto::einvoice::DecidedCorrection;
use pos_proto::events::BillingBillVoided;
use pos_proto::ids::{BillId, ReasonCodeId};
use pos_proto::reason_codes::ReasonAction;
use pos_proto::wire_enum::Open;

use crate::app::{
    AppError, Approval, BillRecord, Edge, PaidBill, SettledReceipt, SettledVoidView, TillScope,
};

/// What a void after payment reads of the bill before it decides: the bill, its receipt and what
/// its tenders put on it.
pub(super) struct PaidRecord {
    pub(super) bill: BillRecord,
    pub(super) receipt: Option<SettledReceipt>,
    paid: PaidBill,
    taking_shift_open: bool,
}

impl<S: EventStore> Edge<S> {
    /// Voids a bill that was paid, whole, citing a reason from the store's `VOID_BILL` list
    /// (ADR-0184 decisions 2–8).
    ///
    /// What no approval changes is decided first — the bill must be settled, inside the store's
    /// `billing.settled_void_window`, and carry no issued e-invoice unless the store's
    /// `einvoice.invoiced_correction` route lets the till void it (ADR-0245) — so nobody is asked for a PIN
    /// for a void that would be refused anyway. Then `billing.bill.void_settled`, held directly or
    /// approved by a holder's code and PIN. The bill's cash leaves the drawer of the till the void is
    /// done at (ADR-0167), refused as cash into that drawer would be; every other tender is
    /// recorded as reversed; tips are untouched. `billing.bill.voided` is stamped with that drawer's
    /// shift and committed with the approval's override in one transaction, so a replay gives the
    /// same cash back from the same drawer.
    ///
    /// A bill voided with an issued invoice is corrected by the cloud from `billing.bill.voided`, as
    /// `einvoice.correction_kind` says; the step-up below records the approving manager.
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
        // Read before the facts are built, so no projection guard is held across another lock.
        let invoiced = self.invoiced_void(bill_id, &record, &ctx);
        let facts = SettledVoid {
            pin_verified: false,
            window: session.billing.settled_void_window(),
            bill_business_date: record
                .receipt
                .as_ref()
                .map_or(ctx.business_date, |receipt| receipt.business_date),
            taking_shift_open: record.taking_shift_open,
            // A bill with an issued e-invoice is corrected by the route the store chose (ADR-0245).
            e_invoice_issued: invoiced.is_some(),
            invoiced: invoiced.unwrap_or_default(),
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

        let voided = self.settled_voided(bill_id, reason_code_id, facts.e_invoice_issued);
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

    /// The void of a paid bill, saying which correction kind the store applied where the bill had an
    /// issued e-invoice, so the cloud corrects with that kind
    /// ([ADR-0256](../../../../../docs/adr/0256-a-voided-bills-invoice-is-corrected-by-the-kind-the-store-applied.md)).
    fn settled_voided(
        &self,
        bill_id: BillId,
        reason_code_id: ReasonCodeId,
        invoiced: bool,
    ) -> BillingBillVoided {
        BillingBillVoided {
            bill_id,
            reason_code_id,
            correction_kind: invoiced
                .then(|| Open::from_known(self.session().einvoice.correction_kind())),
        }
    }

    /// What the store's `einvoice` route decides the void of an invoiced bill on (ADR-0245): its
    /// total, which a bill with no receipt cannot be within a limit of, and its invoice's age.
    pub(super) fn invoiced_void(
        &self,
        bill_id: BillId,
        record: &PaidRecord,
        ctx: &DecisionCtx,
    ) -> Option<InvoicedVoid> {
        let einvoice = &self.session().einvoice;
        let now_ms = u64::try_from(ctx.now.as_milliseconds_since_epoch()).unwrap_or(0);
        let issued_ms = self.lock_projection().invoices.issued_ms(bill_id)?;
        Some(InvoicedVoid {
            route: einvoice.invoiced_correction(),
            bill_total_minor: record
                .receipt
                .as_ref()
                .map_or(i64::MAX, |receipt| receipt.total_due.amount_minor),
            manager_limit_minor: einvoice.manager_correction_limit(),
            invoice_age_minutes: i64::try_from(now_ms.saturating_sub(issued_ms) / 60_000)
                .unwrap_or(i64::MAX),
            cancel_window_minutes: einvoice.cancel_window_minutes(),
            approved_by_hq: einvoice
                .correction_decision(bill_id)
                .is_some_and(DecidedCorrection::approved),
        })
    }

    /// The bill, its receipt, what it was paid with and whether the shift its settle was stamped
    /// with is still open, read under one lock.
    pub(super) fn paid_record(
        &self,
        bill_id: BillId,
        ctx: &DecisionCtx,
    ) -> Result<PaidRecord, AppError> {
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
pub(in crate::app::bills) mod tests;
