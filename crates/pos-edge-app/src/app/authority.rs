// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! Who may act ([ADR-0158](../../../../docs/adr/0158-the-till-enforces-each-persons-own-permissions.md)):
//! each person's own permissions in the decision context, the check for an act no decision covers,
//! a manager's step-up approval and the override record it owes the log.

use pos_core::business_date::derive_business_date;
use pos_core::decision::{Actor, DecisionCtx};
use pos_core::error::DomainError;
use pos_core::permission::{Permission, PermissionSet};
use pos_ports::event_store::EventStore;
use pos_proto::ClockSource;
use pos_proto::events::SecurityPermissionOverridden;
use pos_proto::ids::EmployeeId;
use pos_proto::money::Money;
use pos_proto::text::PermissionKey;

use super::{AppError, Approval, Edge};
use crate::auth::{LockoutPolicy, SignIn};

impl<S: EventStore> Edge<S> {
    /// Resolves a step-up for one `permission`, returning the approver and whether a PIN was
    /// verified.
    ///
    /// The permission's own `pin_required` decides whether an approval is needed at all, so the
    /// registry stays the single source of truth: flipping `pin: true` on a permission changes what
    /// the till demands, with no second list to update.
    ///
    /// What the approval grants is deliberately narrow: **exactly the permission being exercised**,
    /// not the approver's whole set. A manager approving a void must not incidentally hand the
    /// server their discount ceiling for the same act.
    pub(in crate::app) fn resolve_step_up(
        &self,
        ctx: &mut DecisionCtx,
        permission: Permission,
        approval: Option<&Approval>,
    ) -> Result<Option<EmployeeId>, AppError> {
        let session = self.session();
        // Where the store enforces each person's own set (ADR-0158 decision 4), the actor's role
        // says whether this act needs an approver: none for a permission held directly, one for a
        // permission held with approval, and a refusal for one not held at all. Elsewhere the
        // catalogue's PIN flag decides, as it always has.
        if session.permissions_enforced {
            if ctx.granted_directly.contains(permission) {
                return Ok(None);
            }
            if !session
                .staff
                .permissions_with_approval_for(ctx.actor.employee_id)
                .contains(permission)
            {
                return Err(AppError::Domain(DomainError::PermissionDenied {
                    permission: permission.meta().id,
                }));
            }
        } else if !permission.meta().pin_required {
            return Ok(None);
        }
        let approval = approval.ok_or(AppError::ApprovalRequired)?;
        // An unknown code is refused without touching the lockout, as sign-in refuses one: there is
        // nobody to count the failure against, and a probe cannot tell a real code from a missing one.
        let (approver, phc, held) = session
            .staff
            .approver(&approval.code)
            .ok_or(AppError::ApprovalRefused)?;
        // The store's own lockout numbers (ADR-0160), the same ones its sign-in counts against.
        match self.pin_lockout.authenticate(
            approver,
            phc,
            &approval.pin,
            self.clock.now(),
            LockoutPolicy::of(&session.session_settings),
        ) {
            SignIn::Ok => {}
            SignIn::Wrong { .. } => return Err(AppError::ApprovalRefused),
            SignIn::LockedOut { .. } => return Err(AppError::ApproverLockedOut),
        }
        if !held.contains(permission) {
            return Err(AppError::ApprovalRefused);
        }
        // Approving is vouching for somebody else (decision 5). Only once the store enforces each
        // person's own set: until then a manager alone on shift has nobody else to approve their own
        // void, and approving it themselves is how they do it.
        if session.permissions_enforced && approver == ctx.actor.employee_id {
            return Err(AppError::ApprovalRefused);
        }
        ctx.granted = ctx.granted.with(permission);
        Ok(Some(approver))
    }

    /// The `security.permission.overridden` a verified step-up owes the log.
    ///
    /// `docs/pos-spec.md` §11.4 makes a manager-PIN override a fraud control, and until roadmap B2.2
    /// this event had no producer anywhere in the tree — the control had no auditable record at all,
    /// exactly as the event's own declaration says. `exceeded_by` is `None` here: a void is not an
    /// amount over a ceiling, it is an act a role does not hold, and inventing a figure would be
    /// worse than saying there is none.
    pub(in crate::app) fn override_record(
        permission: Permission,
        approver: EmployeeId,
        exceeded_by: Option<Money>,
    ) -> SecurityPermissionOverridden {
        SecurityPermissionOverridden {
            permission_key: PermissionKey::new(permission.meta().id),
            approver_employee_id: approver,
            exceeded_by,
        }
    }

    /// Refuses `actor` an act no decision covers unless they hold `permission`, as a decision would
    /// refuse it (ADR-0158).
    ///
    /// For the routes that write no event and so decide nothing — printing a pre-bill — but still
    /// do what a role grants. The same set and the same refusal as every command, so a till cannot
    /// tell the two apart.
    ///
    /// # Errors
    ///
    /// [`AppError::Domain`] carrying `PermissionDenied` when the actor does not hold `permission`,
    /// or [`AppError::Clock`] if the business date cannot be derived.
    pub fn authorise(&self, actor: Actor, permission: Permission) -> Result<(), AppError> {
        self.decision_ctx(actor)?.require(permission)?;
        Ok(())
    }

    /// Assembles the decision context from the clock and the session.
    pub(in crate::app) fn decision_ctx(&self, actor: Actor) -> Result<DecisionCtx, AppError> {
        let now = self.clock.now();
        let session = self.session();
        let business_date = derive_business_date(now, &session.timezone, session.cutoff)
            .map_err(|_| AppError::Clock)?;
        // Each person's own set once the store enforces it (ADR-0158): what their roles grant
        // directly, and nothing for an identity the roster does not know. What they hold only with
        // approval joins a context one act at a time, in `resolve_step_up`. Until then the
        // store-wide set, in which nobody holds anything directly, so a PIN-flagged permission asks
        // for a PIN as it always has.
        let (granted, granted_directly) = if session.permissions_enforced {
            let own = session
                .staff
                .permissions_for(actor.employee_id)
                .unwrap_or(PermissionSet::EMPTY);
            (own, own)
        } else {
            (session.granted, PermissionSet::EMPTY)
        };
        Ok(DecisionCtx {
            now,
            business_date,
            actor,
            granted,
            granted_directly,
            capabilities: session.capabilities,
            connectivity: session.connectivity,
            currency: session.currency,
        })
    }
}

#[cfg(test)]
mod tests {
    use pos_fakes::FakeStore;
    use pos_proto::ids::{OrderId, StationId, TableId};
    use pos_proto::ulid::Ulid;
    use pos_proto::{SalesChannel, TableState};

    use crate::app::tests::{a_line, actor, edge, vnd};
    use crate::app::{Edge, EdgeSession};

    /// Narrows what the store grants to everything but `withheld`: what a person whose roles lack
    /// it will hold once each person's own set decides (ADR-0158).
    fn withhold(edge: &Edge<FakeStore>, withheld: pos_core::permission::Permission) {
        edge.apply_session(EdgeSession {
            granted: pos_core::permission::Permission::ALL
                .iter()
                .copied()
                .filter(|permission| *permission != withheld)
                .collect(),
            ..(*edge.session()).clone()
        });
    }

    /// Asserts the edge refused for want of exactly `permission`.
    fn denied_for<T: core::fmt::Debug>(
        result: Result<T, super::AppError>,
        permission: pos_core::permission::Permission,
    ) {
        match result {
            Err(super::AppError::Domain(pos_core::error::DomainError::PermissionDenied {
                permission: id,
            })) => assert_eq!(id, permission.meta().id),
            other => panic!(
                "expected a refusal for {}, got {other:?}",
                permission.meta().id
            ),
        }
    }

    /// The commands that decide nothing in `pos-core` still need their permission at the edge
    /// (ADR-0158 decision 2): taking an order, opening and printing the bill, bumping a ticket and
    /// opening a shift. Each is refused naming it, and nothing is written.
    #[test]
    fn each_command_the_core_does_not_decide_still_needs_its_permission() {
        use pos_core::permission::Permission;
        pos_fakes::executor::run_ready(async {
            let table = TableId::new(Ulid::from_u128(150));

            let taking = edge();
            taking
                .seat_table(actor(), table, None)
                .await
                .expect("seats");
            withhold(&taking, Permission::AddLine);
            denied_for(
                taking.add_line(actor(), table, a_line()).await,
                Permission::AddLine,
            );
            denied_for(
                taking
                    .open_counter_order(actor(), SalesChannel::Takeaway)
                    .await,
                Permission::AddLine,
            );
            let order_id = taking
                .order_for_table(table)
                .expect("the seated table holds an order");
            assert!(
                taking.order_line_ids(order_id).is_empty(),
                "no line was written"
            );

            let billing = edge();
            billing
                .seat_table(actor(), table, None)
                .await
                .expect("seats");
            billing
                .add_line(actor(), table, a_line())
                .await
                .expect("adds");
            withhold(&billing, Permission::OpenBill);
            denied_for(
                billing.open_bill(actor(), table).await,
                Permission::OpenBill,
            );
            denied_for(
                billing.authorise(actor(), Permission::OpenBill),
                Permission::OpenBill,
            );
            assert_eq!(billing.table_state(table), TableState::Occupied);

            let kitchen = edge();
            withhold(&kitchen, Permission::BumpTicket);
            denied_for(
                kitchen
                    .bump_ticket(
                        actor(),
                        OrderId::new(Ulid::from_u128(151)),
                        StationId::new(Ulid::from_u128(152)),
                        vec![pos_proto::ids::OrderLineId::new(Ulid::from_u128(153))],
                    )
                    .await,
                Permission::BumpTicket,
            );
            assert!(kitchen.bumped_line_ids().is_empty());

            let drawer = edge();
            withhold(&drawer, Permission::OpenShift);
            denied_for(
                drawer.open_shift(actor(), vnd(500_000)).await,
                Permission::OpenShift,
            );
            assert!(drawer.current_shift().is_none());
        });
    }
}
