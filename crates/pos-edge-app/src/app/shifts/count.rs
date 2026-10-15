// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! Counting a drawer's shift, blind where the store says so.

use pos_core::decision::{Actor, ShiftCommand, decide_shift};
use pos_core::permission::Permission;
use pos_ports::event_store::EventStore;
use pos_proto::events::CashShiftCounted;
use pos_proto::ids::ShiftId;
use pos_proto::money::Money;

use crate::app::records::another_till;
use crate::app::{AppError, Approval, Edge, ShiftRecord, ShiftView, TillScope};

impl<S: EventStore> Edge<S> {
    /// Records the blind count on a shift (`cash.shift.counted`).
    ///
    /// The counted cash is entered **before** the system reveals what it expected, so this returns
    /// no expectation and no variance — that is what makes the close blind (§11.1). A store that has
    /// turned the blind close off is shown the expectation (`Self::expectation_shown`), never the
    /// variance. `counted_minor` is the physical count in minor units.
    ///
    /// # Errors
    ///
    /// As [`Self::count_shift_at`].
    pub async fn count_shift(
        &self,
        actor: Actor,
        shift_id: ShiftId,
        counted_minor: i64,
    ) -> Result<ShiftView, AppError> {
        self.count_shift_at(actor, TillScope::default(), shift_id, counted_minor, None)
            .await
    }

    /// [`Self::count_shift`] from the till `scope` says the device is: another till's drawer also
    /// needs `cash.shift.manage_other_till`, held directly or approved with a holder's PIN
    /// (ADR-0167 decision 3).
    ///
    /// # Errors
    ///
    /// [`AppError::UnknownShift`] for a shift the edge does not know, [`AppError::Domain`] if the
    /// shift is not open or a permission is not held, the approval refusals of
    /// [`Self::open_shift_at`], or [`AppError`] if the store cannot be written.
    pub async fn count_shift_at(
        &self,
        actor: Actor,
        scope: TillScope,
        shift_id: ShiftId,
        counted_minor: i64,
        approval: Option<&Approval>,
    ) -> Result<ShiftView, AppError> {
        let mut ctx = self.decision_ctx(actor)?;
        let record = self
            .lock_projection()
            .shift(shift_id)
            .ok_or(AppError::UnknownShift)?;
        let approver = self.manage_other_till(&mut ctx, another_till(scope, &record), approval)?;
        let overrider = self.override_assignment(&mut ctx, Some(&record), approval)?;
        let decision = decide_shift(record.state, ShiftCommand::Count { counted_minor }, &ctx)?;

        let counted_amount = Money::new(self.session().currency, counted_minor);
        let payload = CashShiftCounted {
            counted_shift_id: shift_id,
            counted_amount,
            count_time: ctx.now,
        };
        let step_ups = [
            (Permission::ManageOtherTill, approver),
            (Permission::OverrideDrawerAssignment, overrider),
        ];
        self.commit_approved(&ctx, &payload, Some(shift_id), &step_ups)
            .await?;

        self.lock_projection()
            .record_shift_count(shift_id, counted_amount, decision.next_state);
        let counted = ShiftRecord {
            state: decision.next_state,
            counted: Some(counted_amount),
            ..record
        };
        Ok(self.shift_view(shift_id, &counted))
    }
}
