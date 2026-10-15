// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! What reduces a bill: a discount, within the person's ceiling or with an approver's, and waiving a
//! fee.

use pos_core::billing::BillTotals;
use pos_core::decision::{Actor, BillCommand, decide_bill, over_ceiling};
use pos_core::error::DomainError;
use pos_core::machines::{Bill, BillTrigger};
use pos_core::permission::Permission;
use pos_core::{StateMachine, billing};
use pos_ports::event_store::EventStore;
use pos_proto::events::{BillingDiscountApplied, BillingFeeWaived};
use pos_proto::ids::{BillId, FeeId, ReasonCodeId};
use pos_proto::money::Money;
use pos_proto::reason_codes::ReasonAction;
use pos_proto::{Open, ReductionKind};

use crate::app::{AppError, Approval, Edge};

impl<S: EventStore> Edge<S> {
    /// Takes money off a bill before it settles — a bill-level discount (`docs/pos-spec.md` §14.4,
    /// roadmap B2.2).
    ///
    /// `billing.discount.applied` has been defined in `pos-proto`, with exactly this shape, since
    /// the schema was written, and `billing::assemble` has taken a `bill_discount` and allocated it
    /// across tax classes for just as long. Nothing ever emitted the event and the edge passed a
    /// hard-coded zero, so the arithmetic ran on every bill and could only ever produce the same
    /// answer.
    ///
    /// # The ceiling, and why an absent one is zero
    ///
    /// `billing.discount.apply` is granted to a **server** and is not PIN-flagged — its own
    /// description is "apply a discount up to the role's configured ceiling". **No store publishes
    /// a ceiling**: nothing in the cloud authors one and no config node carries one, which is why
    /// `None` goes to the domain here. [`decide_bill`] reads that as zero, so today every discount
    /// needs `billing.discount.override_ceiling` and a manager's PIN. The moment a ceiling is
    /// published this becomes ordinary work for a server with no code change, which is the right
    /// direction for the default to fail in.
    ///
    /// # Errors
    ///
    /// [`AppError::UnknownBill`]; [`AppError::VoidReasonNotValid`] if the reason is not one the
    /// store publishes for a discount; [`AppError::ApprovalRefused`] if the manager's PIN does not
    /// authorise; [`AppError::Domain`] if the bill has settled or been voided, if the actor may not
    /// discount, or if the reductions would take the bill below nothing.
    pub async fn discount_bill(
        &self,
        actor: Actor,
        bill_id: BillId,
        amount: Money,
        reason_code_id: ReasonCodeId,
        approval: Option<&Approval>,
    ) -> Result<BillTotals, AppError> {
        let mut ctx = self.decision_ctx(actor)?;
        let session = self.session();
        let bill = self
            .lock_projection()
            .bill(bill_id)
            .ok_or(AppError::UnknownBill)?;
        if !session
            .reason_codes
            .accepts(reason_code_id, ReasonAction::Discount)
        {
            return Err(AppError::VoidReasonNotValid);
        }

        // The pre-tax base is what there is to reduce: tax follows from it, so reducing past it
        // would make the tax negative before the total was.
        let order = self.bill_snapshot(&bill, &session)?;
        let mut reducible = Money::zero(session.currency);
        for base in &order.class_bases {
            reducible = reducible
                .checked_add(base.amount)
                .map_err(DomainError::from)?;
        }
        let (discount, comp) = bill.reductions(session.currency);
        let applied = discount.checked_add(comp).map_err(DomainError::from)?;

        // What this person may discount on their own, read from the roster the cloud published. Read
        // *before* the step-up, because it is what decides whether a step-up was needed at all.
        let ceiling = session.staff.discount_ceiling_for(actor.employee_id);

        // The step-up is attempted only when the till sent one, exactly as a line void does: the
        // till does not have to know the ceiling rule, and the domain answers with the permission
        // that was missing when it needed one and none came. And only when the discount goes over
        // the ceiling: under it there is nothing to approve, so an approval the till sent anyway is
        // not checked and records no override, which would otherwise carry an `exceeded_by` of
        // zero or less — an audit entry for nothing.
        let over = over_ceiling(amount, ceiling).map_err(AppError::Domain)?;
        let approver = match approval {
            Some(_) if over => {
                self.resolve_step_up(&mut ctx, Permission::OverrideDiscountCeiling, approval)?
            }
            _ => None,
        };

        // The decision is the gate, not a value: a reduction moves the bill nowhere and runs no
        // effect, so there is nothing in it to carry forward. What it does is refuse.
        let _decided = decide_bill(
            bill.state,
            BillCommand::Reduce {
                kind: ReductionKind::Discount,
                amount,
                applied,
                reducible,
                // The acting person's, from their role (ADR-0070's `permissions` node). `None` for a
                // role that configures none, which the domain reads as zero — see the header.
                ceiling,
                pin_verified: approver.is_some(),
            },
            &ctx,
        )?;

        let applied_event = BillingDiscountApplied {
            bill_id,
            // Bill-level: the whole check, not a line on it. A line-level discount is the same
            // event with this set, and is its own change.
            order_line_id: None,
            // A person decided this, not a rule. A campaign-granted discount arrives from the
            // campaign engine and carries its id.
            campaign_id: None,
            kind: Open::from_known(ReductionKind::Discount),
            amount,
            reason_code_id: Some(reason_code_id),
        };
        let (event_envelope, event_message) = self.prepare(&ctx, &applied_event)?;
        let mut envelopes = vec![event_envelope];
        let mut messages = vec![event_message];
        if let Some(approver) = approver {
            // `exceeded_by` carries how far over the ceiling this went: the whole amount when the
            // role configures none (the ceiling is zero, so all of it is over), and the difference
            // when it does. A void's override carries no figure at all because a void is not an
            // amount over anything, and that asymmetry is the point of the field.
            let exceeded_by = match ceiling {
                Some(ceiling) => amount.checked_sub(ceiling).map_err(DomainError::from)?,
                None => amount,
            };
            let record = Self::override_record(
                Permission::OverrideDiscountCeiling,
                approver,
                Some(exceeded_by),
            );
            let (envelope, message) = self.prepare(&ctx, &record)?;
            envelopes.push(envelope);
            messages.push(message);
        }
        self.append_and_publish(envelopes, messages).await?;

        self.lock_projection()
            .add_reduction(bill_id, ReductionKind::Discount, amount)
            .map_err(AppError::Domain)?;

        // The new figure, so the till redraws the bill from the edge's arithmetic rather than
        // subtracting the discount itself and risking a second opinion about the tax.
        let (discount, comp) = self
            .lock_projection()
            .bill(bill_id)
            .ok_or(AppError::UnknownBill)?
            .reductions(session.currency);
        Ok(billing::assemble(&Self::bill_input(
            &session,
            &order,
            &bill.fee_rules,
            &bill.waived_fee_ids,
            (discount, comp),
        ))?)
    }

    /// Waives one of a bill's fees, citing a reason from the store's managed list
    /// ([ADR-0159](../../../../../docs/adr/0159-a-fee-is-configuration.md) decision 5).
    ///
    /// An act, not an edit: the rule stays published and every other bill is charged it, while
    /// this one is charged nothing and taxed nothing for it from now on, through every split and
    /// merge, because the waive follows the fee's id. Only a rule published `waivable` is waived,
    /// and only on a bill still open. [`Permission::WaiveFee`] is PIN-flagged, so where the store
    /// does not enforce each person's own set a holder's code and PIN come with every waive, and
    /// where it does, a person who holds it with approval brings somebody else who holds it
    /// directly (`Self::resolve_step_up`), and one who holds it directly waives alone.
    ///
    /// Everything that could never be waived is refused before anybody is asked to approve it: a
    /// bill that owes nothing any more, a fee the bill does not charge, and a rule that says no.
    ///
    /// Answers with the bill as it now stands, so the till redraws it from the edge's arithmetic.
    ///
    /// # Errors
    ///
    /// [`AppError::UnknownBill`]; [`AppError::ReasonCodeNotValid`] if the reason is not one the
    /// store publishes for waiving a fee; [`AppError::Domain`] with a transition refusal for a bill
    /// that is not open, [`DomainError::FeeNotOnBill`] or [`DomainError::FeeNotWaivable`], or a
    /// refusal of the permission; [`AppError::ApprovalRequired`], [`AppError::ApprovalRefused`] or
    /// [`AppError::ApproverLockedOut`] from the step-up; [`AppError::Port`] if the store cannot be
    /// written.
    pub async fn waive_fee(
        &self,
        actor: Actor,
        bill_id: BillId,
        fee_id: FeeId,
        reason_code_id: ReasonCodeId,
        approval: Option<&Approval>,
    ) -> Result<BillTotals, AppError> {
        let mut ctx = self.decision_ctx(actor)?;
        let session = self.session();
        let bill = self
            .lock_projection()
            .bill(bill_id)
            .ok_or(AppError::UnknownBill)?;
        if !session
            .reason_codes
            .accepts(reason_code_id, ReasonAction::WaiveFee)
        {
            return Err(AppError::ReasonCodeNotValid);
        }
        Bill::step(bill.state, BillTrigger::Reduce).map_err(DomainError::from)?;
        billing::waivable_fee(&bill.fee_rules, &bill.waived_fee_ids, fee_id)?;
        let approver = self.resolve_step_up(&mut ctx, Permission::WaiveFee, approval)?;
        let _decided = decide_bill(
            bill.state,
            BillCommand::WaiveFee {
                pin_verified: approver.is_some(),
            },
            &ctx,
        )?;

        let waived = BillingFeeWaived {
            bill_id,
            fee_id,
            reason_code_id,
        };
        let (envelope, message) = self.prepare(&ctx, &waived)?;
        let mut envelopes = vec![envelope];
        let mut messages = vec![message];
        // One transaction for the waive and the approval that let it through: a log holding the
        // waive without it would show money forgiven by nobody who could forgive it.
        if let Some(approver) = approver {
            let record = Self::override_record(Permission::WaiveFee, approver, None);
            let (envelope, message) = self.prepare(&ctx, &record)?;
            envelopes.push(envelope);
            messages.push(message);
        }
        self.append_and_publish(envelopes, messages).await?;

        self.lock_projection().waive_fee_record(bill_id, fee_id);
        self.bill_totals(bill_id)
    }
}
