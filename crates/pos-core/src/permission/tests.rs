// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The permission catalogue's unit tests.

use super::{
    Grant, Permission, PermissionSet, Role, default_grants, render_matrix_markdown,
    render_snapshot, require,
};
use crate::error::DomainError;

#[test]
fn deny_by_default() {
    // The empty set grants nothing — the property every offline authorisation rests on.
    for permission in Permission::ALL {
        assert!(matches!(
            require(*permission, PermissionSet::EMPTY),
            Err(DomainError::PermissionDenied { .. })
        ));
    }
}

#[test]
fn a_granted_permission_is_allowed_and_carries_its_pin_flag() {
    let granted = PermissionSet::EMPTY.with(Permission::VoidFiredLine);
    let grant = require(Permission::VoidFiredLine, granted).expect("granted");
    assert_eq!(
        grant,
        Grant { pin_required: true },
        "voiding a fired line always needs a PIN"
    );

    let low = PermissionSet::EMPTY.with(Permission::OpenShift);
    let grant = require(Permission::OpenShift, low).expect("granted");
    assert!(!grant.pin_required, "opening a shift does not");
}

#[test]
fn granting_one_permission_does_not_grant_another() {
    let granted = PermissionSet::EMPTY.with(Permission::OpenShift);
    assert!(matches!(
        require(Permission::VoidBill, granted),
        Err(DomainError::PermissionDenied { permission }) if permission == "billing.bill.void"
    ));
}

#[test]
fn the_owner_default_role_gets_everything() {
    // Not a rule the spec mandates, but a sane default and a guard that default_grants works:
    // every permission lists Owner, so the Owner seed is the full catalogue.
    let owner = default_grants(Role::Owner);
    for permission in Permission::ALL {
        assert!(
            owner.contains(*permission),
            "Owner should get {}",
            permission.meta().id
        );
    }
    assert_eq!(owner.len() as usize, Permission::ALL.len());
}

#[test]
fn a_cook_does_not_get_money_permissions() {
    let cook = default_grants(Role::Cook);
    assert!(!cook.contains(Permission::VoidBill));
    assert!(!cook.contains(Permission::IssueRefund));
    assert!(!cook.contains(Permission::OpenDrawerNoSale));
    assert!(!cook.contains(Permission::TakePayment));
    assert!(!cook.contains(Permission::ViewTakings));
    // But does get to record waste, 86 an item and bump a ticket.
    assert!(cook.contains(Permission::RecordWaste));
    assert!(cook.contains(Permission::MarkItemUnavailable));
    assert!(cook.contains(Permission::BumpTicket));
}

#[test]
fn every_id_is_distinct_and_dotted() {
    // ids are the contract in the snapshot, so a duplicate or a malformed one is a real bug.
    let mut ids: Vec<&str> = Permission::ALL.iter().map(|p| p.meta().id).collect();
    let count = ids.len();
    ids.sort_unstable();
    ids.dedup();
    assert_eq!(ids.len(), count, "two permissions share an id");
    for permission in Permission::ALL {
        let id = permission.meta().id;
        let segments: Vec<&str> = id.split('.').collect();
        assert_eq!(segments.len(), 3, "{id} is not domain.resource.action");
        assert!(
            segments.iter().all(|s| {
                !s.is_empty() && s.bytes().all(|b| b.is_ascii_lowercase() || b == b'_')
            }),
            "{id} is not lower snake_case"
        );
    }
}

#[test]
fn high_risk_money_actions_require_a_pin() {
    // A weak but useful invariant: the money vectors the fraud section names must be
    // PIN-flagged, so nobody quietly ships one that is not.
    for permission in [
        Permission::VoidFiredLine,
        Permission::ApplyComp,
        Permission::VoidBill,
        Permission::IssueRefund,
        Permission::OpenDrawerNoSale,
        Permission::OverridePrice,
        Permission::WaiveFee,
    ] {
        assert!(
            permission.meta().pin_required,
            "{} must need a PIN",
            permission.meta().id
        );
    }
}

#[test]
fn a_fee_is_waived_by_default_by_whoever_may_exceed_the_discount_ceiling() {
    // ADR-0159 decision 5: waiving a fee forgives money as a discount above the ceiling does,
    // so the same roles hold it directly out of the box, and migration 0076 gave it to the
    // roles that existed on the same terms.
    assert_eq!(
        Permission::WaiveFee.meta().default_roles,
        Permission::OverrideDiscountCeiling.meta().default_roles
    );
    assert_eq!(Permission::WaiveFee.meta().id, "billing.fee.waive");
}

#[test]
fn another_tills_drawer_is_managed_by_default_by_whoever_may_open_one_without_a_sale() {
    // ADR-0167 decision 3: a PIN-flagged permission held out of the box by the roles that open
    // a drawer without a sale, so migration 0084's rule, directly where a role grants that one
    // directly and with approval otherwise, gives an existing tenant what a new one starts with.
    let manage = Permission::ManageOtherTill.meta();
    assert_eq!(manage.id, "cash.shift.manage_other_till");
    assert!(
        manage.pin_required,
        "a cashier needs a holder's approval for it"
    );
    assert_eq!(manage.group.as_token(), "CASH_AND_SHIFTS");
    assert_eq!(
        manage.risk,
        Permission::CloseShift.meta().risk,
        "a drawer's session, as closing one's own is"
    );
    assert_eq!(
        manage.default_roles,
        Permission::OpenDrawerNoSale.meta().default_roles
    );
    assert!(!default_grants(Role::Cashier).contains(Permission::ManageOtherTill));
}

#[test]
fn an_assigned_drawer_is_overridden_by_default_by_whoever_may_open_one_without_a_sale() {
    // ADR-0167 decision 9: held out of the box by the roles that open a drawer without a sale,
    // so migration 0086's rule, directly where a role grants that one directly and with approval
    // otherwise, gives an existing tenant what a new one starts with.
    let over = Permission::OverrideDrawerAssignment.meta();
    assert_eq!(over.id, "cash.drawer.override_assignment");
    assert!(
        over.pin_required,
        "a cashier needs a holder's approval for it"
    );
    assert_eq!(over.group.as_token(), "CASH_AND_SHIFTS");
    assert_eq!(
        over.risk,
        Permission::OpenDrawerNoSale.meta().risk,
        "cash another person answers for, as a drawer opened without a sale is"
    );
    assert_eq!(
        over.default_roles,
        Permission::OpenDrawerNoSale.meta().default_roles
    );
    assert!(!default_grants(Role::Cashier).contains(Permission::OverrideDrawerAssignment));
}

#[test]
fn the_takings_are_seen_by_default_by_whoever_runs_the_floors_money() {
    // ADR-0160 decision 2: who sees takings is a permission. Out of the box the owner, the
    // manager and the supervisor; never a cashier, a server or a cook, until an owner says so.
    let takings = Permission::ViewTakings.meta();
    assert_eq!(takings.id, "reports.takings.view");
    assert_eq!(
        takings.default_roles,
        [Role::Supervisor, Role::Manager, Role::Owner]
    );
    assert!(!takings.pin_required, "seeing a figure asks nobody's PIN");
    assert_eq!(takings.group.as_token(), "REPORTS");
    assert_eq!(takings.risk.as_token(), "MEDIUM");
    for role in [Role::Cashier, Role::Server, Role::Cook] {
        assert!(
            !default_grants(role).contains(Permission::ViewTakings),
            "{}",
            role.as_token()
        );
    }
}

#[test]
fn the_approvers_who_close_a_shift_are_exactly_the_roles_that_see_takings_by_default() {
    // ADR-0160 decision 2: migration 0079 gave takings to every existing role that grants
    // closing a shift and a PIN-flagged permission directly. Applied to the starting roles, the
    // same rule must pick exactly the roles that hold takings out of the box, so an existing
    // tenant and a new one agree on who sees the store's money.
    let mut approvers_who_close: Vec<Role> = Role::ALL
        .iter()
        .copied()
        .filter(|role| {
            let direct = default_grants(*role);
            direct.contains(Permission::CloseShift)
                && Permission::ALL.iter().any(|permission| {
                    permission.meta().pin_required && direct.contains(*permission)
                })
        })
        .collect();
    approvers_who_close.sort_unstable();
    let mut see_takings = Permission::ViewTakings.meta().default_roles.to_vec();
    see_takings.sort_unstable();
    assert_eq!(approvers_who_close, see_takings);
}

#[test]
fn the_snapshot_matches_the_committed_file() {
    let rendered = render_snapshot();
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../docs/snapshots/permissions.txt");
    if std::env::var_os("POS_UPDATE_SNAPSHOTS").is_some() {
        std::fs::write(&path, &rendered).expect("write permissions snapshot");
    }
    let committed = std::fs::read_to_string(&path)
        .expect("docs/snapshots/permissions.txt exists; regenerate with POS_UPDATE_SNAPSHOTS=1");
    assert_eq!(
        rendered, committed,
        "regenerate with POS_UPDATE_SNAPSHOTS=1 cargo test -p pos-core"
    );
}

#[test]
fn the_matrix_doc_matches_the_committed_file() {
    let rendered = render_matrix_markdown();
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs/permissions.md");
    if std::env::var_os("POS_UPDATE_SNAPSHOTS").is_some() {
        std::fs::write(&path, &rendered).expect("write permission matrix");
    }
    let committed = std::fs::read_to_string(&path)
        .expect("docs/permissions.md exists; regenerate with POS_UPDATE_SNAPSHOTS=1");
    assert_eq!(
        rendered, committed,
        "regenerate with POS_UPDATE_SNAPSHOTS=1 cargo test -p pos-core"
    );
}

#[test]
fn reading_a_shifts_x_is_the_manager_and_the_owners_without_a_pin() {
    // ADR-0187 decision 4: reading is not an act on money, so no PIN, and out of the box only the
    // manager and the owner see the cash a blind-close drawer should hold.
    let read_x = Permission::ReadShiftX.meta();
    assert_eq!(read_x.id, "reports.shift.read_x");
    assert!(!read_x.pin_required);
    assert_eq!(read_x.default_roles, [Role::Manager, Role::Owner]);
    for role in [Role::Cashier, Role::Server, Role::Supervisor, Role::Cook] {
        assert!(
            !default_grants(role).contains(Permission::ReadShiftX),
            "{role:?}"
        );
    }
}
