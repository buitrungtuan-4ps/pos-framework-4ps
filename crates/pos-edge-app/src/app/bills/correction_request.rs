// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! A store manager asks HQ to correct a paid bill's issued e-invoice
//! ([ADR-0245](../../../../../docs/adr/0245-an-invoiced-bill-is-corrected-by-the-route-the-store-chooses.md)
//! route b, [ADR-0250](../../../../../docs/adr/0250-a-store-learns-a-correction-decision-on-its-einvoice-node.md)).
//!
//! The request moves no money and issues nothing: it records `fiscal.correction.requested`, which
//! reaches the cloud's queue. Once HQ approves, the cloud publishes the decision on the store's
//! `einvoice` node, and the till may then void the bill as any other.

use pos_core::decision::settled_void::decide_correction_request;
use pos_core::decision::{Actor, DecisionCtx};
use pos_core::permission::Permission;
use pos_ports::event_store::EventStore;
use pos_proto::einvoice::{CorrectionRequest, EInvoiceKind};
use pos_proto::events::{FiscalCorrectionDecided, FiscalCorrectionRequested};
use pos_proto::ids::{BillId, ReasonCodeId};
use pos_proto::money::Money;
use pos_proto::reason_codes::ReasonAction;
use pos_proto::time::Timestamp;
use pos_proto::wire_enum::Open;

use crate::app::{AppError, Approval, Edge};

/// A correction request as it was recorded, for the till to show.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CorrectionRequested {
    /// The bill.
    pub bill_id: BillId,
    /// The amount asked back: the bill's total.
    pub amount: Money,
    /// The kind the cloud will issue once it is approved.
    pub kind: EInvoiceKind,
    /// When it was asked.
    pub requested_time: Timestamp,
}

impl<S: EventStore> Edge<S> {
    /// Records a manager's request that HQ correct a paid bill's issued invoice (ADR-0250
    /// decision 3), under `billing.bill.void_settled` held directly or approved with a code and PIN.
    ///
    /// The reason must be one of the store's `VOID_BILL` reasons, and is required while
    /// `einvoice.correction_reason_required` is on.
    ///
    /// # Errors
    ///
    /// [`AppError::UnknownBill`]; [`AppError::VoidReasonNotValid`] for a missing reason the store
    /// requires, or one it does not hold for voiding a bill; [`AppError::Domain`] with a transition
    /// refusal for a bill that is not settled, `CorrectionNotNeeded`, `EInvoiceIssued` under
    /// `HQ_ONLY`, or the permission's refusal; the step-up's refusals; [`AppError::Port`] if the
    /// store cannot be written.
    pub async fn request_invoice_correction(
        &self,
        actor: Actor,
        bill_id: BillId,
        reason_code_id: Option<ReasonCodeId>,
        approval: Option<&Approval>,
    ) -> Result<CorrectionRequested, AppError> {
        let mut ctx = self.decision_ctx(actor)?;
        let session = self.session();
        let record = self.paid_record(bill_id, &ctx)?;
        let reason_ok = reason_code_id
            .map_or(!session.einvoice.correction_reason_required(), |reason| {
                session.reason_codes.accepts(reason, ReasonAction::VoidBill)
            });
        if !reason_ok {
            return Err(AppError::VoidReasonNotValid);
        }
        let invoiced = self.invoiced_void(bill_id, &record, &ctx);
        // Decided as if the act were held first, so nobody is asked for a PIN for a refusal.
        let probe = held(&ctx);
        decide_correction_request(record.bill.state, invoiced, true, &probe)?;
        let approver = self.resolve_step_up(&mut ctx, Permission::VoidSettledBill, approval)?;
        decide_correction_request(record.bill.state, invoiced, approver.is_some(), &ctx)?;

        let amount = record
            .receipt
            .as_ref()
            .map_or_else(|| Money::zero(ctx.currency), |receipt| receipt.total_due);
        let kind = session.einvoice.correction_kind();
        let requested = FiscalCorrectionRequested {
            request: CorrectionRequest {
                bill_id,
                amount,
                kind: Open::from_known(kind),
                reason_code_id,
            },
        };
        let (envelope, message) = self.prepare(&ctx, &requested)?;
        let mut envelopes = vec![envelope];
        let mut messages = vec![message];
        if let Some(approved_by) = approver {
            let record = Self::override_record(Permission::VoidSettledBill, approved_by, None);
            let (envelope, message) = self.prepare(&ctx, &record)?;
            envelopes.push(envelope);
            messages.push(message);
        }
        self.append_and_publish(envelopes, messages).await?;
        self.lock_projection().invoices.fold_requested(bill_id);
        Ok(CorrectionRequested {
            bill_id,
            amount,
            kind,
            requested_time: ctx.now,
        })
    }
}

impl<S: EventStore> Edge<S> {
    /// Records `fiscal.correction.decided` for each decision the store's `einvoice` node holds on a
    /// bill this store asked about and has not recorded yet (ADR-0250 decision 3), stamped by the
    /// box; answers how many it recorded. Folded from the log, so a restart records none twice.
    ///
    /// # Errors
    ///
    /// The first store or clock failure; the decisions before it stay recorded.
    pub async fn record_correction_decisions(&self) -> Result<usize, AppError> {
        let published = self.session().einvoice.correction_decisions.clone();
        if published.is_empty() {
            return Ok(0);
        }
        let unrecorded = self.lock_projection().invoices.unrecorded(&published);
        let mut recorded = 0;
        for decision in unrecorded {
            let ctx = self.box_ctx()?;
            let decided = FiscalCorrectionDecided { decision };
            let (envelope, message) = self.prepare_on(&ctx, &decided, None)?;
            self.append_and_publish(vec![envelope], vec![message])
                .await?;
            self.lock_projection()
                .invoices
                .fold_decided(&decided.decision);
            recorded += 1;
        }
        Ok(recorded)
    }
}

/// `ctx` as if `billing.bill.void_settled` were held directly.
fn held(ctx: &DecisionCtx) -> DecisionCtx {
    let mut probe = ctx.clone();
    probe.granted = probe.granted.with(Permission::VoidSettledBill);
    probe.granted_directly = probe.granted_directly.with(Permission::VoidSettledBill);
    probe
}

#[cfg(test)]
mod tests;
