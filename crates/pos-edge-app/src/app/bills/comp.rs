// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! Giving a line away: a comp, the whole line, with a reason and an approver
//! ([ADR-0219](../../../../../docs/adr/0219-a-comp-and-a-price-override-each-need-a-reason-and-an-approver.md)
//! decision 1).

use pos_core::billing::BillTotals;
use pos_core::decision::{Actor, BillCommand, decide_bill};
use pos_core::error::DomainError;
use pos_core::permission::Permission;
use pos_ports::event_store::EventStore;
use pos_proto::events::BillingCompApplied;
use pos_proto::ids::{BillId, OrderLineId, ReasonCodeId};
use pos_proto::money::Money;
use pos_proto::reason_codes::ReasonAction;
use pos_proto::{OrderLineState, ReductionKind};

use super::reductions::reducible_base;
use crate::app::{AppError, Approval, BillRecord, Edge};

impl<S: EventStore> Edge<S> {
    /// Gives one line of an open bill away (ADR-0219 decision 1).
    ///
    /// The amount is the edge's, never the till's: the line's whole total as it was captured, its
    /// modifiers inside it. A comp is a whole line, at most once, and lowers the taxable base as a
    /// discount does (`billing::assemble` allocates it so); the food is still consumed as sold.
    /// [`Permission::ApplyComp`] is PIN-flagged, so the step-up runs exactly as a fee waive's does,
    /// and the approval is recorded beside the comp in one transaction.
    ///
    /// Everything that could never be comped is refused before anybody is asked to approve it: a
    /// line the bill does not cover, a voided one, one already comped, and a reason the store does
    /// not publish for a comp.
    ///
    /// # Errors
    ///
    /// [`AppError::UnknownBill`]; [`AppError::UnknownLine`] for a line the bill does not cover or
    /// one voided; [`AppError::AlreadyComped`]; [`AppError::ReasonCodeNotValid`];
    /// [`AppError::ApprovalRequired`], [`AppError::ApprovalRefused`] or
    /// [`AppError::ApproverLockedOut`] from the step-up; [`AppError::Domain`] for a bill that is not
    /// open, a missing permission, or a comp that would take the bill below nothing;
    /// [`AppError::Port`] if the store cannot be written.
    pub async fn comp_line(
        &self,
        actor: Actor,
        bill_id: BillId,
        order_line_id: OrderLineId,
        reason_code_id: ReasonCodeId,
        approval: Option<&Approval>,
    ) -> Result<BillTotals, AppError> {
        let mut ctx = self.decision_ctx(actor)?;
        let session = self.session();
        let bill = self
            .lock_projection()
            .bill(bill_id)
            .ok_or(AppError::UnknownBill)?;
        let amount = self.compable_line(&bill, order_line_id)?;
        if !session
            .reason_codes
            .accepts(reason_code_id, ReasonAction::Comp)
        {
            return Err(AppError::ReasonCodeNotValid);
        }
        let order = self.bill_snapshot(&bill, &session)?;
        let reducible = reducible_base(&order.class_bases, session.currency)?;
        let (discount, comp) = bill.reductions(session.currency);
        let applied = discount.checked_add(comp).map_err(DomainError::from)?;

        let approver = self.resolve_step_up(&mut ctx, Permission::ApplyComp, approval)?;
        let _decided = decide_bill(
            bill.state,
            BillCommand::Reduce {
                kind: ReductionKind::Comp,
                amount,
                applied,
                reducible,
                // A comp is manager-only by its own permission; the discount ceiling is not read.
                ceiling: None,
                pin_verified: approver.is_some(),
            },
            &ctx,
        )?;

        let comped = BillingCompApplied {
            bill_id,
            order_line_id,
            amount,
            reason_code_id,
        };
        let (envelope, message) = self.prepare(&ctx, &comped)?;
        let mut envelopes = vec![envelope];
        let mut messages = vec![message];
        // One transaction for the comp and the approval that let it through: a log holding the
        // comp without it would show food given away by nobody who could give it.
        if let Some(approver) = approver {
            let record = Self::override_record(Permission::ApplyComp, approver, None);
            let (envelope, message) = self.prepare(&ctx, &record)?;
            envelopes.push(envelope);
            messages.push(message);
        }
        self.append_and_publish(envelopes, messages).await?;

        self.lock_projection()
            .apply_comp(&comped)
            .map_err(AppError::Domain)?;
        self.bill_totals(bill_id)
    }

    /// What comping `order_line_id` on `bill` takes off: the line's whole total, once the line is
    /// one the bill covers, still sold, and not comped before.
    fn compable_line(
        &self,
        bill: &BillRecord,
        order_line_id: OrderLineId,
    ) -> Result<Money, AppError> {
        let projection = self.lock_projection();
        if !projection.covered_lines(bill).contains(&order_line_id) {
            return Err(AppError::UnknownLine);
        }
        let line = projection
            .line(order_line_id)
            .ok_or(AppError::UnknownLine)?;
        if line.state == OrderLineState::Voided {
            return Err(AppError::UnknownLine);
        }
        if line.comped {
            return Err(AppError::AlreadyComped);
        }
        Ok(line.line_total)
    }
}
