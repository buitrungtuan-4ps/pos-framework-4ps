// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! Cash paid into and out of a drawer outside a sale, and the drawer opened without one.

use pos_core::decision::Actor;
use pos_core::permission::Permission;
use pos_ports::event_store::EventStore;
use pos_proto::ShiftState;
use pos_proto::events::{CashDrawerOpened, CashDrawerPaidIn, CashDrawerPaidOut};
use pos_proto::ids::{DeviceId, ReasonCodeId, ShiftId};
use pos_proto::money::Money;
use pos_proto::reason_codes::ReasonAction;

use crate::app::{AppError, Approval, CashMovement, Edge, ShiftView, TillScope};

impl<S: EventStore> Edge<S> {
    /// Records cash paid into or out of the drawer outside a sale, against the shift trading now
    /// (`cash.drawer.paid_in` or `cash.drawer.paid_out`,
    /// [ADR-0165](../../../docs/adr/0165-cash-paid-in-and-out-is-counted-in-the-drawer-and-a-no-sale-opening-needs-a-manager.md) decision 1).
    ///
    /// The amount moves what the close expects, which is why a counted shift refuses it: the blind
    /// count was made against the drawer as it stood. A paid out larger than the drawer should hold
    /// is **not** refused, because the refusal would tell the cashier roughly what it holds, and the
    /// close is blind; the variance shows it. `amount` is above zero, which the route checks, as it
    /// checks a discount's. The kick is the caller's, after this commits.
    ///
    /// # Errors
    ///
    /// As [`Self::record_cash_movement_at`].
    pub async fn record_cash_movement(
        &self,
        actor: Actor,
        shift_id: ShiftId,
        movement: CashMovement,
        amount: Money,
        reason_code_id: ReasonCodeId,
    ) -> Result<ShiftView, AppError> {
        self.record_cash_movement_at(
            actor,
            TillScope::default(),
            shift_id,
            movement,
            amount,
            reason_code_id,
            None,
        )
        .await
    }

    /// [`Self::record_cash_movement`] at the till `scope` says the device is. Where the store keeps
    /// a drawer per till, a paid in or out is made at the till whose drawer it springs, on that
    /// drawer's shift ([ADR-0167](../../../docs/adr/0167-a-till-has-its-own-cash-drawer.md)
    /// decision 3). A drawer assigned to somebody else also takes the override, held directly or
    /// approved for this act (decision 9).
    ///
    /// # Errors
    ///
    /// [`AppError::Domain`] without `cash.movement.record`, [`AppError::CashReasonNotValid`] for a
    /// reason the store does not hold for this act, [`AppError::NotATill`] at a device that is no
    /// till where the store keeps a drawer per till, [`AppError::UnknownShift`] for a shift the edge
    /// does not know, [`AppError::ShiftNotOpen`] for one that is not trading, has been counted or is
    /// another till's drawer's, [`AppError::DrawerAssignedToAnother`] and the approval refusals of
    /// [`Self::open_shift_at`] for somebody else's drawer, or [`AppError`] if the store cannot be
    /// written.
    pub async fn record_cash_movement_at(
        &self,
        actor: Actor,
        scope: TillScope,
        shift_id: ShiftId,
        movement: CashMovement,
        amount: Money,
        reason_code_id: ReasonCodeId,
        approval: Option<&Approval>,
    ) -> Result<ShiftView, AppError> {
        let mut ctx = self.decision_ctx(actor)?;
        ctx.require(Permission::RecordCashMovement)?;
        if !self
            .session()
            .reason_codes
            .accepts(reason_code_id, movement.reason_action())
        {
            return Err(AppError::CashReasonNotValid);
        }
        let drawer = self.cash_drawer(scope)?;
        let (record, trading) = {
            let projection = self.lock_projection();
            let record = projection.shift(shift_id);
            let trading = record
                .as_ref()
                .is_some_and(|record| projection.open_shift_of(record.till) == Some(shift_id));
            (record, trading)
        };
        let record = record.ok_or(AppError::UnknownShift)?;
        // Another till's drawer's shift is not open at this one: the cash would go into one drawer
        // and the kick spring another.
        if !trading || record.state != ShiftState::Open || record.till != drawer {
            return Err(AppError::ShiftNotOpen);
        }
        self.refuse_after_day_end(shift_id, &record)?;
        let overrider = self.override_assignment(&mut ctx, Some(&record), approval)?;
        let step_ups = [(Permission::OverrideDrawerAssignment, overrider)];
        // On the shift's own drawer, and stamped with it, whichever drawer that is (ADR-0167).
        let shift = Some(shift_id);
        let till = record.till;
        match movement {
            CashMovement::PaidIn => {
                let paid = CashDrawerPaidIn {
                    amount,
                    reason_code_id,
                    terminal_device_id: till,
                };
                self.commit_approved(&ctx, &paid, shift, &step_ups).await?;
            }
            CashMovement::PaidOut => {
                let paid = CashDrawerPaidOut {
                    amount,
                    reason_code_id,
                    terminal_device_id: till,
                };
                self.commit_approved(&ctx, &paid, shift, &step_ups).await?;
            }
        }
        let record = {
            let mut projection = self.lock_projection();
            projection.record_movement(shift_id, movement, amount)?;
            projection.shift(shift_id).unwrap_or(record)
        };
        Ok(self.shift_view(shift_id, &record))
    }

    /// Opens the cash drawer outside a sale (`cash.drawer.opened` with `standalone: true`,
    /// [ADR-0165](../../../docs/adr/0165-cash-paid-in-and-out-is-counted-in-the-drawer-and-a-no-sale-opening-needs-a-manager.md) decision 3).
    ///
    /// `cash.drawer.open_no_sale` asks for a PIN, so a manager types their badge code and PIN in
    /// place, and the opening carries a reason from the store's list for it. Allowed with or without
    /// an open shift, because it moves no cash; the envelope names the shift when one is trading.
    /// The kick is the caller's, after this commits, as a receipt's paper is.
    ///
    /// # Errors
    ///
    /// As [`Self::open_drawer_no_sale_at`].
    pub async fn open_drawer_no_sale(
        &self,
        actor: Actor,
        reason_code_id: ReasonCodeId,
        approval: Option<&Approval>,
    ) -> Result<(), AppError> {
        self.open_drawer_no_sale_at(actor, TillScope::default(), reason_code_id, approval)
            .await
            .map(|_drawer| ())
    }

    /// [`Self::open_drawer_no_sale`] at the till `scope` says the device is, answering with the till
    /// whose drawer to open: `None` for the store's one drawer, and where the store keeps one per
    /// till the device's own, which the opening names and whose shift it is stamped with
    /// ([ADR-0167](../../../docs/adr/0167-a-till-has-its-own-cash-drawer.md) decisions 3 and 5).
    ///
    /// # Errors
    ///
    /// [`AppError::CashReasonNotValid`] for a reason the store does not hold for opening the drawer,
    /// [`AppError::NotATill`] at a device that is no till where the store keeps a drawer per till,
    /// [`AppError::ApprovalRequired`], [`AppError::ApprovalRefused`] or
    /// [`AppError::ApproverLockedOut`] for the PIN, [`AppError::Domain`] without the permission,
    /// [`AppError::DrawerAssignedToAnother`] for a drawer assigned to somebody else, whose override
    /// the same approver may give (ADR-0167 decision 9), or [`AppError`] if the store cannot be
    /// written.
    pub async fn open_drawer_no_sale_at(
        &self,
        actor: Actor,
        scope: TillScope,
        reason_code_id: ReasonCodeId,
        approval: Option<&Approval>,
    ) -> Result<Option<DeviceId>, AppError> {
        let mut ctx = self.decision_ctx(actor)?;
        if !self
            .session()
            .reason_codes
            .accepts(reason_code_id, ReasonAction::DrawerOpen)
        {
            return Err(AppError::CashReasonNotValid);
        }
        let drawer = self.cash_drawer(scope)?;
        let approver = self.resolve_step_up(&mut ctx, Permission::OpenDrawerNoSale, approval)?;
        ctx.require(Permission::OpenDrawerNoSale)?;
        let (shift, record) = {
            let projection = self.lock_projection();
            let shift = projection.open_shift_of(drawer);
            (shift, shift.and_then(|shift_id| projection.shift(shift_id)))
        };
        // Somebody else's drawer is theirs to open too, where the store assigns drawers.
        let overrider = self.override_assignment(&mut ctx, record.as_ref(), approval)?;
        let opened = CashDrawerOpened {
            standalone: true,
            reason_code_id: Some(reason_code_id),
            terminal_device_id: drawer,
        };
        let step_ups = [
            (Permission::OpenDrawerNoSale, approver),
            (Permission::OverrideDrawerAssignment, overrider),
        ];
        self.commit_approved(&ctx, &opened, shift, &step_ups)
            .await?;
        Ok(drawer)
    }
}
