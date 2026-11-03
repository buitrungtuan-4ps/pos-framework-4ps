// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! Opening a drawer's shift with its starting float.

use pos_core::cash_count::CountBreakdown;
use pos_core::decision::Actor;
use pos_core::permission::Permission;
use pos_ports::event_store::EventStore;
use pos_proto::events::CashShiftOpened;
use pos_proto::ids::{EmployeeId, ShiftId};
use pos_proto::money::Money;

use crate::app::{AppError, Approval, Edge, ShiftRecord, ShiftView, TillScope};

impl<S: EventStore> Edge<S> {
    /// Opens the store's one drawer's shift with a starting float: [`Self::open_shift_at`] for a
    /// request that names no till from a device that is none.
    ///
    /// # Errors
    ///
    /// As [`Self::open_shift_at`].
    pub async fn open_shift(
        &self,
        actor: Actor,
        opening_float: Money,
    ) -> Result<ShiftView, AppError> {
        self.open_shift_at(actor, TillScope::default(), opening_float, None, None)
            .await
    }

    /// Opens a cash shift with a starting float (`cash.shift.opened`) on the drawer `scope` names
    /// ([ADR-0167](../../../../../docs/adr/0167-a-till-has-its-own-cash-drawer.md) decisions 2 and 3).
    ///
    /// Where the store keeps one drawer it is every device's, and one shift is open at a time.
    /// Where it keeps one per till, each opens on its own: the device's own till's drawer, or the
    /// till the request names, which for another till's also needs `cash.shift.manage_other_till`,
    /// held directly or approved with a holder's PIN. Subsequent events carry the store's shift's
    /// id until it closes.
    ///
    /// Where the store assigns drawers (decision 9), the session is the starter's at the drawer's
    /// own till, and the store's one drawer is too; another till's is `assignee`'s, who must be on
    /// the store's roster. A manager starting a drawer for somebody else names them, rather than
    /// answering for cash they will not handle and locking the person who will out of it, so one
    /// naming nobody is refused. Elsewhere, and where drawers are shared, `assignee` is ignored,
    /// as a named till is under one drawer.
    ///
    /// # Errors
    ///
    /// [`AppError::NotATill`] where the store keeps a drawer per till and neither the device nor the
    /// request names one of its tills, [`AppError::ShiftAlreadyOpen`] if that drawer's shift is open,
    /// [`AppError::AssigneeRequired`] for another till's drawer under assigned drawers naming nobody
    /// on the roster, [`AppError::Domain`] without `cash.shift.open`, or without
    /// `cash.shift.manage_other_till` for another till's drawer, [`AppError::ApprovalRequired`],
    /// [`AppError::ApprovalRefused`] or [`AppError::ApproverLockedOut`] for its PIN, or
    /// [`AppError`] if the store cannot be written.
    pub async fn open_shift_at(
        &self,
        actor: Actor,
        scope: TillScope,
        opening_float: Money,
        approval: Option<&Approval>,
        assignee: Option<EmployeeId>,
    ) -> Result<ShiftView, AppError> {
        let float = (opening_float, CountBreakdown::default());
        self.open_shift_counted(actor, scope, float, approval, assignee, None)
            .await
    }

    /// [`Self::open_shift_at`] with the float counted note by note
    /// ([ADR-0186](../../../../../docs/adr/0186-a-drawer-is-counted-by-denomination.md)), which
    /// `cash.shift.opened` records beside the amount.
    ///
    /// # Errors
    ///
    /// Where the opening takes the drawer over at a handover
    /// ([ADR-0187](../../../../../docs/adr/0187-a-shift-is-handed-over-at-the-till-and-read-there.md)
    /// decision 2), `handed_over_from` names the shift just closed on that drawer, which
    /// `cash.shift.opened` records: the float is then that shift's cash carried on.
    ///
    /// # Errors
    ///
    /// [`AppError::CountDoesNotAddUp`] where the breakdown does not come to the float, before
    /// anything else is checked; [`AppError::NotTheShiftHandedOver`] where `handed_over_from` is not
    /// the shift that drawer closed last; and otherwise as [`Self::open_shift_at`].
    pub async fn open_shift_counted(
        &self,
        actor: Actor,
        scope: TillScope,
        (opening_float, breakdown): (Money, CountBreakdown),
        approval: Option<&Approval>,
        assignee: Option<EmployeeId>,
        handed_over_from: Option<ShiftId>,
    ) -> Result<ShiftView, AppError> {
        if !breakdown.adds_up_to(opening_float) {
            return Err(AppError::CountDoesNotAddUp);
        }
        // A replaced machine does not open a new trading day (ADR-0123). Before the
        // already-open check, so a box that is superseded says so rather than reporting whichever
        // state it happens to be in.
        self.refuse_if_superseded()?;
        let (till, another) = self.drawer_named(scope)?;
        {
            let projection = self.lock_projection();
            if projection.open_shift_of(till).is_some() {
                return Err(AppError::ShiftAlreadyOpen);
            }
            if handed_over_from.is_some() && handed_over_from != projection.last_closed_of(till) {
                return Err(AppError::NotTheShiftHandedOver);
            }
        }
        let owner = self.owner_of_start(actor, another, assignee)?;
        let mut ctx = self.decision_ctx(actor)?;
        let approver = self.manage_other_till(&mut ctx, another, approval)?;
        ctx.require(Permission::OpenShift)?;
        let shift_id = ShiftId::new(self.next_ulid());

        let payload = CashShiftOpened {
            opened_shift_id: shift_id,
            opening_float,
            // Absent for the store's one drawer, which writes today's bytes (decision 5).
            terminal_device_id: till,
            // Absent where drawers are shared, which writes today's bytes too (decision 9).
            assigned_employee_id: owner,
            denomination_counts: breakdown.denomination_counts,
            uncounted_amount: breakdown.uncounted_amount,
            // Absent for a float brought to the drawer, which writes today's bytes (ADR-0187).
            handed_over_from_shift_id: handed_over_from,
        };
        let step_ups = [(Permission::ManageOtherTill, approver)];
        self.commit_approved(&ctx, &payload, self.current_shift_id(), &step_ups)
            .await?;

        let currency = self.session().currency;
        let record = ShiftRecord::opened(
            opening_float,
            currency,
            (till, owner),
            ctx.now,
            ctx.business_date,
        );
        let view = self.shift_view(shift_id, &record);
        self.lock_projection().open_shift_record(shift_id, record);
        Ok(view)
    }
}
