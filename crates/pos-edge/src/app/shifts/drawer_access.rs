// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! Which cash drawer an act is on, and what acting on it takes
//! ([ADR-0167](../../../../../docs/adr/0167-a-till-has-its-own-cash-drawer.md) decisions 1, 3, 8 and
//! 9): the drawer model the edge runs, the drawers it keeps, the drawer a request names, and the
//! step-ups another till's drawer and somebody else's drawer take, committed beside the act.

use pos_core::decision::{Actor, DecisionCtx};
use pos_core::permission::Permission;
use pos_ports::event_store::EventStore;
use pos_proto::envelope::EventPayload;
use pos_proto::ids::{DeviceId, EmployeeId, ShiftId};
use pos_proto::money::Money;
use pos_proto::shift::{DrawerAccess, DrawerModel};
use pos_proto::text::DisplayName;

use crate::app::{
    AppError, Approval, DrawerView, DrawersView, Edge, ShiftRecord, TillScope, another_till,
    is_till,
};

impl<S: EventStore> Edge<S> {
    /// The drawer model this edge runs ([ADR-0167](../../../../../docs/adr/0167-a-till-has-its-own-cash-drawer.md) decisions 1 and
    /// 8): the model its open shifts were opened under while any is open, and the store's published
    /// `shift.drawer_model` once none is, so a change of model waits for every drawer to close.
    #[must_use]
    pub fn drawer_model(&self) -> DrawerModel {
        let open = self.lock_projection().open_model();
        open.unwrap_or_else(|| self.session().shift.drawer_model())
    }

    /// Every drawer the store keeps, for `GET /api/shifts` (ADR-0167 decision 3): the store's one,
    /// or one for each till its `devices` node lists and any other whose shift is still open, each
    /// with its default float and its shift while one is open, as blind as [`Self::current_shift`].
    ///
    /// Where the store has turned the blind close off, a drawer's expectation shows to the device
    /// that is its till, and another till's only to `actor` where their own role grants
    /// `cash.shift.manage_other_till` directly, so a cashier does not read the bar's drawer.
    #[must_use]
    pub fn drawers(&self, actor: Actor, scope: TillScope) -> DrawersView {
        let session = self.session();
        let sees_others = session
            .staff
            .grants(actor.employee_id, Permission::ManageOtherTill);
        let model = self.drawer_model();
        let published = session.shift.drawer_model();
        let float = |minor| Money::new(session.currency, minor);
        let store_float = float(session.shift.opening_float_minor());
        let mut drawers: Vec<(Option<DeviceId>, Option<DisplayName>, Money)> = Vec::new();
        if model == DrawerModel::PerTerminal {
            for till in session
                .devices
                .devices()
                .iter()
                .filter(|device| is_till(device))
            {
                let default_float = till.opening_float_minor().map_or(store_float, float);
                drawers.push((Some(till.device_id), Some(till.name.clone()), default_float));
            }
            let mut unlisted: Vec<Option<DeviceId>> =
                self.lock_projection().open_shifts.keys().copied().collect();
            unlisted.retain(|till| drawers.iter().all(|(listed, _, _)| listed != till));
            unlisted.sort_unstable();
            drawers.extend(unlisted.into_iter().map(|till| (till, None, store_float)));
        } else {
            drawers.push((None, None, store_float));
        }
        let drawers = drawers
            .into_iter()
            .map(|(till, name, default_float)| {
                let open = {
                    let projection = self.lock_projection();
                    projection
                        .open_shift_of(till)
                        .and_then(|shift_id| Some((shift_id, projection.shift(shift_id)?)))
                };
                let shift = open.as_ref().map(|(shift_id, record)| {
                    let mut view = self.shift_view(*shift_id, record);
                    if another_till(scope, record) && !sees_others {
                        view.expected_amount = None;
                    }
                    view
                });
                DrawerView {
                    till,
                    name,
                    default_float,
                    opened_time: open.map(|(_, record)| record.opened_time),
                    shift,
                }
            })
            .collect();
        DrawersView {
            model,
            waiting: (published != model).then_some(published),
            own: scope.own.filter(|_| model == DrawerModel::PerTerminal),
            drawers,
            access: session.shift.drawer_access(),
        }
    }

    /// The person on the store's roster under `code`, whom a drawer started for another till may be
    /// given to where the store assigns drawers (ADR-0167 decision 9): `None` for a code nobody on
    /// it has, or one with no id. The till types a code, as it does an approver's, because it keeps
    /// no names.
    #[must_use]
    pub fn employee_with_code(&self, code: &str) -> Option<EmployeeId> {
        self.session().staff.get(code)?.employee_id
    }

    /// The drawer `scope` names under the model the edge runs, and whether it is another till's than
    /// the device's own: the store's where it keeps one drawer, whatever the request names.
    pub(super) fn drawer_named(
        &self,
        scope: TillScope,
    ) -> Result<(Option<DeviceId>, bool), AppError> {
        if self.drawer_model() != DrawerModel::PerTerminal {
            return Ok((None, false));
        }
        let till = scope.named.or(scope.own).ok_or(AppError::NotATill)?;
        if !self.is_published_till(till) {
            return Err(AppError::NotATill);
        }
        Ok((Some(till), scope.own != Some(till)))
    }

    /// What acting on another till's drawer takes beside the act's own permission:
    /// `cash.shift.manage_other_till`, PIN-flagged, so held directly or approved for this one act
    /// (ADR-0167 decision 3, ADR-0158 decisions 4 and 5). The approver, where one approved.
    pub(super) fn manage_other_till(
        &self,
        ctx: &mut DecisionCtx,
        another: bool,
        approval: Option<&Approval>,
    ) -> Result<Option<EmployeeId>, AppError> {
        if !another {
            return Ok(None);
        }
        let approver = self.resolve_step_up(ctx, Permission::ManageOtherTill, approval)?;
        ctx.require(Permission::ManageOtherTill)?;
        Ok(approver)
    }

    /// Whose a drawer starting now is, where the store assigns drawers (ADR-0167 decision 9): the
    /// starter's, or for `another` till's drawer `assignee`'s, who must be on the store's roster.
    /// `None` where drawers are shared.
    pub(super) fn owner_of_start(
        &self,
        actor: Actor,
        another: bool,
        assignee: Option<EmployeeId>,
    ) -> Result<Option<EmployeeId>, AppError> {
        let session = self.session();
        if session.shift.drawer_access() != DrawerAccess::Assigned {
            return Ok(None);
        }
        if !another {
            return Ok(Some(actor.employee_id));
        }
        match assignee {
            Some(person) if session.staff.permissions_for(person).is_some() => Ok(Some(person)),
            _ => Err(AppError::AssigneeRequired),
        }
    }

    /// What acting on `record`'s drawer takes where it is assigned to somebody other than the person
    /// acting ([ADR-0167](../../../../../docs/adr/0167-a-till-has-its-own-cash-drawer.md) decision 9):
    /// `cash.drawer.override_assignment`, PIN-flagged, so held directly or approved for this one act
    /// (ADR-0158 decisions 4 and 5). The approver, where one approved. Nothing for a drawer with no
    /// owner, which is every drawer where drawers are shared, nor for the owner's own act.
    ///
    /// Asked without an approver of a person who does not hold it directly, the refusal is
    /// [`AppError::DrawerAssignedToAnother`], which names nobody, so the till asks for one.
    pub(super) fn override_assignment(
        &self,
        ctx: &mut DecisionCtx,
        record: Option<&ShiftRecord>,
        approval: Option<&Approval>,
    ) -> Result<Option<EmployeeId>, AppError> {
        let permission = Permission::OverrideDrawerAssignment;
        match record.and_then(|record| record.owner) {
            Some(owner) if owner != ctx.actor.employee_id => {}
            _ => return Ok(None),
        }
        if approval.is_none() && !ctx.granted_directly.contains(permission) {
            return Err(AppError::DrawerAssignedToAnother);
        }
        let approver = self.resolve_step_up(ctx, permission, approval)?;
        ctx.require(permission)?;
        Ok(approver)
    }

    /// Commits `payload` stamped with `shift`, and beside it the override each approver owes the
    /// log for the permission they approved, in one append.
    pub(super) async fn commit_approved<P: EventPayload + serde::Serialize>(
        &self,
        ctx: &DecisionCtx,
        payload: &P,
        shift: Option<ShiftId>,
        step_ups: &[(Permission, Option<EmployeeId>)],
    ) -> Result<(), AppError> {
        let mut prepared = vec![self.prepare_on(ctx, payload, shift)?];
        for (permission, approver) in step_ups {
            if let Some(approver) = approver {
                let overridden = Self::override_record(*permission, *approver, None);
                prepared.push(self.prepare_on(ctx, &overridden, shift)?);
            }
        }
        let (envelopes, messages) = prepared.into_iter().unzip();
        self.append_and_publish(envelopes, messages).await
    }
}
