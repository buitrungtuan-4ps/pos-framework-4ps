// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! Closing a shift, or several counted drawers together, the variance reason a close may owe, and
//! the chain head a close anchors.

use pos_core::billing;
use pos_core::decision::{Actor, DecisionCtx, Effect, ShiftCommand, decide_shift};
use pos_core::error::DomainError;
use pos_core::permission::Permission;
use pos_ports::event_store::EventStore;
use pos_proto::events::{CashShiftClosed, StoreChainAnchored};
use pos_proto::ids::{ReasonCodeId, ShiftId};
use pos_proto::money::Money;
use pos_proto::reason_codes::ReasonAction;
use pos_proto::shift::CloseReport;

use crate::app::records::{another_till, by_name_and_rate, closed_view};
use crate::app::{
    AppError, Approval, ClosedDrawers, CombinedReport, DrawerFigures, Edge, ShiftRecord,
    ShiftReport, ShiftView, TillScope,
};

impl<S: EventStore> Edge<S> {
    /// Closes a counted shift (`cash.shift.closed`), revealing the expected amount and the variance.
    ///
    /// The expectation is the shift's cash roll-up — opening float plus the cash its bills took — and
    /// is revealed only now, after the blind count. The variance is `counted − expected`; negative is
    /// short. Closing surfaces [`Effect::PrintShiftReport`] on the view for the caller to run.
    ///
    /// # Errors
    ///
    /// As [`Self::close_shift_at`].
    pub async fn close_shift(
        &self,
        actor: Actor,
        shift_id: ShiftId,
    ) -> Result<ShiftView, AppError> {
        self.close_shift_at(actor, TillScope::default(), shift_id, None, None)
            .await
    }

    /// [`Self::close_shift`] from the till `scope` says the device is: another till's drawer also
    /// needs `cash.shift.manage_other_till`, held directly or approved with a holder's PIN
    /// (ADR-0167 decision 3). The report it asks for is that drawer's alone.
    ///
    /// A drawer over or short by more than the store's `shift.variance_reason_minor` closes with
    /// `reason`, which `cash.shift.closed` records, where the store's list offers a reason for that
    /// (decision 12). Otherwise `reason` is not asked for, and is ignored.
    ///
    /// # Errors
    ///
    /// [`AppError::UnknownShift`] for a shift the edge does not know, [`AppError::Domain`] if the
    /// shift has not been counted (a blind close cannot skip the count) or a permission is not held,
    /// the approval refusals of [`Self::open_shift_at`], [`AppError::VarianceReasonRequired`] for a
    /// variance owed a reason and given none, [`AppError::CashReasonNotValid`] for a reason the
    /// store's list does not hold for it, or [`AppError`] if the store cannot be written.
    pub async fn close_shift_at(
        &self,
        actor: Actor,
        scope: TillScope,
        shift_id: ShiftId,
        approval: Option<&Approval>,
        reason: Option<ReasonCodeId>,
    ) -> Result<ShiftView, AppError> {
        let mut ctx = self.decision_ctx(actor)?;
        let record = self
            .lock_projection()
            .shift(shift_id)
            .ok_or(AppError::UnknownShift)?;
        let approver = self.manage_other_till(&mut ctx, another_till(scope, &record), approval)?;
        let overrider = self.override_assignment(&mut ctx, Some(&record), approval)?;
        let decision = decide_shift(record.state, ShiftCommand::Close, &ctx)?;
        let (mut payload, report) = self.closing(shift_id, &record)?;
        if self.owes_variance_reason(report.variance) {
            let Some(reason) = reason else {
                let owing = self.owing_view(shift_id, &record, &report);
                return Err(AppError::VarianceReasonRequired(vec![owing]));
            };
            payload.reason_code_id = Some(self.variance_reason(reason)?);
        }
        let step_ups = [
            (Permission::ManageOtherTill, approver),
            (Permission::OverrideDrawerAssignment, overrider),
        ];
        self.commit_approved(&ctx, &payload, Some(shift_id), &step_ups)
            .await?;
        self.anchor_chain(&ctx).await?;

        self.lock_projection().close_shift_record(shift_id);
        // A drawer closed on its own prints its own report unless the store prints none at a
        // close, whether or not it combines several closed at once (ADR-0167 decision 10).
        let print = decision.effects.contains(&Effect::PrintShiftReport)
            && self.session().shift.close_report() != CloseReport::None;
        Ok(closed_view(&record, report, print))
    }

    /// Closes several counted drawers in one act: a `cash.shift.closed` for each, all in one
    /// append, so every one closes or none does
    /// ([ADR-0167](../../../docs/adr/0167-a-till-has-its-own-cash-drawer.md) decision 10).
    ///
    /// Each drawer closes as [`Self::close_shift_at`] closes one: counted already, under
    /// `cash.shift.close`, and another till's under `cash.shift.manage_other_till` too. One approval
    /// covers the act: its code and PIN are checked once, and each other till's drawer it covers is
    /// recorded with an override of its own, stamped with that drawer's shift. A shift named twice
    /// closes once.
    ///
    /// Where the store's `shift.close_report` combines them, the drawers print as one slip,
    /// [`ClosedDrawers::combined`]. Where it prints per drawer each prints its own report, and where
    /// it prints none none does.
    ///
    /// A drawer whose over or short owes a reason closes with the one `reasons` gives its shift, as
    /// [`Self::close_shift_at`] closes with one (ADR-0167 decision 12).
    ///
    /// # Errors
    ///
    /// [`AppError::UnknownShift`] for a shift the edge does not know, [`AppError::Domain`] for one
    /// that is not counted or a permission not held, the approval refusals of
    /// [`Self::open_shift_at`], [`AppError::CashReasonNotValid`] for a reason the store's list does
    /// not hold for a variance, [`AppError::VarianceReasonRequired`] naming every drawer still owed
    /// one, or [`AppError`] if the store cannot be written. Nothing closes on any of them.
    pub async fn close_shifts_at(
        &self,
        actor: Actor,
        scope: TillScope,
        shift_ids: &[ShiftId],
        approval: Option<&Approval>,
        reasons: &[(ShiftId, ReasonCodeId)],
    ) -> Result<ClosedDrawers, AppError> {
        let mut ctx = self.decision_ctx(actor)?;
        let mut named: Vec<(ShiftId, ShiftRecord)> = Vec::with_capacity(shift_ids.len());
        {
            let projection = self.lock_projection();
            for shift_id in shift_ids {
                if named.iter().all(|(seen, _)| seen != shift_id) {
                    let record = projection.shift(*shift_id).ok_or(AppError::UnknownShift)?;
                    named.push((*shift_id, record));
                }
            }
        }
        let another = named.iter().any(|(_, record)| another_till(scope, record));
        let approver = self.manage_other_till(&mut ctx, another, approval)?;
        // One override for every drawer named that is somebody else's, as one approver covers
        // every other till's (ADR-0167 decision 9).
        let someone_elses =
            |record: &ShiftRecord| record.owner.is_some_and(|owner| owner != actor.employee_id);
        let assigned = named
            .iter()
            .map(|(_, record)| record)
            .find(|record| someone_elses(record));
        let overrider = self.override_assignment(&mut ctx, assigned, approval)?;
        let mode = self.session().shift.close_report();
        let mut prepared = Vec::with_capacity(named.len());
        let mut closed = Vec::with_capacity(named.len());
        let mut owing = Vec::new();
        for (shift_id, record) in &named {
            let decision = decide_shift(record.state, ShiftCommand::Close, &ctx)?;
            let (mut payload, report) = self.closing(*shift_id, record)?;
            if self.owes_variance_reason(report.variance) {
                let Some((_, reason)) = reasons.iter().find(|(shift, _)| shift == shift_id) else {
                    owing.push(self.owing_view(*shift_id, record, &report));
                    continue;
                };
                payload.reason_code_id = Some(self.variance_reason(*reason)?);
            }
            prepared.push(self.prepare_on(&ctx, &payload, Some(*shift_id))?);
            if let Some(approver) = approver.filter(|_| another_till(scope, record)) {
                let overridden = Self::override_record(Permission::ManageOtherTill, approver, None);
                prepared.push(self.prepare_on(&ctx, &overridden, Some(*shift_id))?);
            }
            if let Some(approver) = overrider.filter(|_| someone_elses(record)) {
                let overridden =
                    Self::override_record(Permission::OverrideDrawerAssignment, approver, None);
                prepared.push(self.prepare_on(&ctx, &overridden, Some(*shift_id))?);
            }
            let print = decision.effects.contains(&Effect::PrintShiftReport)
                && mode == CloseReport::PerDrawer;
            closed.push(closed_view(record, report, print));
        }
        // Every drawer or none: one still owed a reason closes none, and the refusal names each.
        if !owing.is_empty() {
            return Err(AppError::VarianceReasonRequired(owing));
        }
        let combined = if mode == CloseReport::Combined {
            let reports: Vec<ShiftReport> = closed
                .iter()
                .filter_map(|view| view.report.clone())
                .collect();
            let mut tax_components = Vec::new();
            for component in reports.iter().flat_map(|report| &report.tax_components) {
                billing::add_component(&mut tax_components, component.clone())?;
            }
            by_name_and_rate(&mut tax_components);
            Some(CombinedReport {
                totals: DrawerFigures::total(&reports, self.session().currency)?,
                tax_components,
                drawers: named
                    .iter()
                    .zip(reports)
                    .map(|((_, record), report)| (self.till_name(record.till), report))
                    .collect(),
            })
        } else {
            None
        };
        if prepared.is_empty() {
            return Ok(ClosedDrawers {
                shifts: closed,
                combined: None,
            });
        }
        let (envelopes, messages) = prepared.into_iter().unzip();
        self.append_and_publish(envelopes, messages).await?;
        self.anchor_chain(&ctx).await?;
        {
            let mut projection = self.lock_projection();
            for (shift_id, _) in &named {
                projection.close_shift_record(*shift_id);
            }
        }
        Ok(ClosedDrawers {
            shifts: closed,
            combined,
        })
    }

    /// What closing `record`'s counted shift writes, its `cash.shift.closed`, and the report of
    /// the figures it closes on.
    fn closing(
        &self,
        shift_id: ShiftId,
        record: &ShiftRecord,
    ) -> Result<(CashShiftClosed, ShiftReport), AppError> {
        // Reaching Close means the shift was counted, so the count is present; the fallback never
        // fires but keeps this off any panic path.
        let counted_amount = record
            .counted
            .unwrap_or_else(|| Money::zero(self.session().currency));
        let expected_amount = record.expected()?;
        let variance = counted_amount
            .checked_sub(expected_amount)
            .map_err(DomainError::from)?;
        let payload = CashShiftClosed {
            closed_shift_id: shift_id,
            expected_amount,
            counted_amount,
            variance,
            terminal_device_id: record.till,
            // Absent unless the variance owes a reason, which writes today's bytes (decision 12).
            reason_code_id: None,
        };
        let mut tax_components = record.tax_components.clone();
        by_name_and_rate(&mut tax_components);
        let report = ShiftReport {
            shift_id,
            opening_float: record.opening_float,
            cash_collected: record.cash_collected,
            paid_in: record.paid_in,
            paid_out: record.paid_out,
            expected_amount,
            counted_amount,
            variance,
            tax_components,
        };
        Ok((payload, report))
    }

    /// Whether a close over or short by `variance` owes a reason (ADR-0167 decision 12): the store's
    /// `shift.variance_reason_minor` is above 0, the variance is beyond it either way, and the
    /// store's list offers a reason for a variance. A store whose list offers none still closes.
    fn owes_variance_reason(&self, variance: Money) -> bool {
        let session = self.session();
        let limit = session.shift.variance_reason_minor();
        limit > 0
            && variance.amount_minor.unsigned_abs() > limit.unsigned_abs()
            && session
                .reason_codes
                .for_action(ReasonAction::CashVariance)
                .next()
                .is_some()
    }

    /// `reason`, where the store's list holds it for a drawer's over or short.
    ///
    /// # Errors
    ///
    /// [`AppError::CashReasonNotValid`] where it does not.
    fn variance_reason(&self, reason: ReasonCodeId) -> Result<ReasonCodeId, AppError> {
        if self
            .session()
            .reason_codes
            .accepts(reason, ReasonAction::CashVariance)
        {
            Ok(reason)
        } else {
            Err(AppError::CashReasonNotValid)
        }
    }

    /// `record`'s counted drawer as a close that owes a reason shows it: still counted, with the
    /// close's expectation and variance, which its count has already fixed (ADR-0167 decision 12).
    fn owing_view(
        &self,
        shift_id: ShiftId,
        record: &ShiftRecord,
        report: &ShiftReport,
    ) -> ShiftView {
        ShiftView {
            expected_amount: Some(report.expected_amount),
            variance: Some(report.variance),
            ..self.shift_view(shift_id, record)
        }
    }

    /// Publishes the head of this store's hash chain
    /// ([ADR-0131](../../../docs/adr/0131-a-chained-event-log.md) decision 4).
    ///
    /// The anchor, and the reason the chain is worth anything against a determined hand. A chain
    /// verifies happily after its tail has been cut off, and after an edit whose later links were
    /// all re-derived — the algorithm is in the source, so anyone holding the file can rebuild one
    /// that checks out. What a store cannot do is rewrite what the cloud has already received.
    ///
    /// Published **after** the shift's own close event, so the anchor covers it. It therefore
    /// reports the chain as it stood before the anchor itself was written, because a record cannot
    /// contain its own hash. A sale landing between the read and the commit leaves it a record or
    /// two behind, which is safe: it still extends the previous anchor and the next covers the gap.
    /// The guarantee reaches back to the **last anchor**, not to the last event, and that is the
    /// cost of anchoring at a boundary a human attends to rather than on every write.
    ///
    /// A store whose adapter keeps no chain publishes nothing, which is the honest outcome rather
    /// than an anchor over a chain that does not exist.
    ///
    /// # Errors
    ///
    /// [`AppError`] if the head cannot be read or the event cannot be committed. A shift close that
    /// reaches this point has already committed; a failure here is reported to the caller and the
    /// close stands, because refusing to close a counted shift over a missing anchor would strand
    /// an operator at the end of the night.
    async fn anchor_chain(&self, ctx: &DecisionCtx) -> Result<(), AppError> {
        let Some(anchor) = self
            .store
            .chain_head(self.identity.store_id)
            .await
            .map_err(AppError::Port)?
        else {
            return Ok(());
        };
        self.commit_and_publish(
            ctx,
            &StoreChainAnchored {
                chain_seq: anchor.seq,
                chain_head: anchor.head,
                unchained: anchor.unchained,
            },
        )
        .await
    }
}
