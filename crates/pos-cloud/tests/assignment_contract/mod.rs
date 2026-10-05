// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The [`AssignmentStore`] contract, held by every implementation of the seam: the in-memory fake in
//! `tests/cloud.rs`, and `PostgresPeople` against a real database in
//! `tests/assignment_store_postgres.rs` (behind the `integration` feature).
//!
//! An assignment reaches one store, a store group or every store of the tenant
//! ([ADR-0158](../../../../docs/adr/0158-the-till-enforces-each-persons-own-permissions.md)
//! decision 3), so the contract is about reach as much as rows: a store's read lists every
//! assignment that reaches it, a group's reach follows its membership as it changes, a tenant-wide
//! one reaches a store nobody has named yet, a person holds one assignment per store, per group and
//! tenant-wide — a second is refused as a duplicate and leaves the first as it was — and nothing
//! crosses a tenant. The group's membership is written through the
//! [`StoreGroupStore`] seam, the one the group routes write, because that is where it lives.

use pos_cloud::people::{
    Assignment, AssignmentId, AssignmentScope, AssignmentStore, AssignmentStoreError, EmployeeId,
    NewAssignment, RoleTemplateId,
};
use pos_cloud::registry::EntityStatus;
use pos_cloud::store_groups::{StoreGroup, StoreGroupId, StoreGroupStore};
use pos_cloud::version::{UpdateOutcome, Version};
use pos_proto::ids::{StoreId, TenantId};
use pos_proto::ulid::Ulid;

/// The ids one run mints, each `seed` plus a small number, so a database that keeps rows from an
/// earlier run never sees one of them twice.
struct Ids(u128);

impl Ids {
    fn ulid(&self, n: u128) -> Ulid {
        Ulid::from_u128(self.0 + n)
    }
    fn tenant(&self, n: u128) -> TenantId {
        TenantId::new(self.ulid(n))
    }
    fn store(&self, n: u128) -> StoreId {
        StoreId::new(self.ulid(0x100 + n))
    }
    fn group(&self, n: u128) -> StoreGroupId {
        StoreGroupId::new(self.ulid(0x200 + n))
    }
    fn employee(&self, n: u128) -> EmployeeId {
        EmployeeId::new(self.ulid(0x300 + n))
    }
    fn role(&self, n: u128) -> RoleTemplateId {
        RoleTemplateId::new(self.ulid(0x400 + n))
    }
    fn assignment(&self, n: u128) -> AssignmentId {
        AssignmentId::new(self.ulid(0x500 + n))
    }
}

/// The ids of a set of assignments, sorted, so two lists compare whatever order each store returns.
fn ids(assignments: &[Assignment]) -> Vec<AssignmentId> {
    let mut ids: Vec<AssignmentId> = assignments.iter().map(|row| row.assignment_id).collect();
    ids.sort_unstable();
    ids
}

/// Writes `members` as a group's whole membership, at the version the group was last written at,
/// and returns the version it is at now.
async fn set_members<G: StoreGroupStore>(
    groups: &G,
    tenant_id: TenantId,
    group_id: StoreGroupId,
    members: &[StoreId],
    at: &Version,
) -> Version {
    match groups
        .set_members(tenant_id, group_id, members, at)
        .await
        .expect("set the membership")
    {
        UpdateOutcome::Updated(version) => version,
        other => panic!("the membership write applies at the version it was given: {other:?}"),
    }
}

/// Runs every rule of the contract against `assignments`, whose group assignments reach the stores
/// `groups` holds. Every id is `seed` plus a small number (see [`Ids`]).
#[expect(
    clippy::too_many_lines,
    reason = "one contract, read top to bottom: each step builds on the rows the one before wrote"
)]
pub(crate) async fn holds<A, G>(assignments: &A, groups: &G, seed: u128)
where
    A: AssignmentStore + Sync,
    G: StoreGroupStore + Sync,
{
    let at = Ids(seed);
    let (mine, other) = (at.tenant(1), at.tenant(2));
    let (north, south, east, opened_later) = (at.store(1), at.store(2), at.store(3), at.store(4));
    let (alice, bao, cam) = (at.employee(1), at.employee(2), at.employee(3));
    let (cashier, lead) = (at.role(1), at.role(2));
    let assign = |n: u128, employee_id: EmployeeId, scope: AssignmentScope, role| NewAssignment {
        assignment_id: at.assignment(n),
        tenant_id: mine,
        employee_id,
        scope,
        role_template_id: role,
    };

    // A group of two stores, through the seam the group routes write.
    let airport = at.group(1);
    let created = groups
        .create_group(&StoreGroup {
            group_id: airport,
            tenant_id: mine,
            name: "Airport".to_owned(),
            status: EntityStatus::Active,
        })
        .await
        .expect("create the group");
    let version = set_members(groups, mine, airport, &[north, south], &created).await;

    // One store, a group and every store: each is written, and reads back with its scope.
    let at_north = assign(1, alice, AssignmentScope::Store(north), cashier);
    let through_airport = assign(2, bao, AssignmentScope::StoreGroup(airport), cashier);
    let everywhere = assign(3, cam, AssignmentScope::Tenant, lead);
    for assignment in [&at_north, &through_airport, &everywhere] {
        assignments.assign(assignment).await.expect("assign");
    }
    for assignment in [&at_north, &through_airport, &everywhere] {
        let read = assignments
            .get(mine, assignment.assignment_id)
            .await
            .expect("read by id")
            .expect("the assignment exists");
        assert_eq!(read.scope, assignment.scope, "it reads back at its scope");
        assert_eq!(read.employee_id, assignment.employee_id);
        assert_eq!(read.role_template_id, assignment.role_template_id);
    }

    // A store's read lists every assignment that reaches it: its own, its groups', the tenant's.
    assert_eq!(
        ids(&assignments
            .list_for_store(mine, north)
            .await
            .expect("by store")),
        vec![at.assignment(1), at.assignment(2), at.assignment(3)],
        "the store's own, the group's and the tenant's"
    );
    assert_eq!(
        ids(&assignments
            .list_for_store(mine, south)
            .await
            .expect("by store")),
        vec![at.assignment(2), at.assignment(3)],
        "a member of the group, without the other store's own"
    );
    assert_eq!(
        ids(&assignments
            .list_for_store(mine, east)
            .await
            .expect("by store")),
        vec![at.assignment(3)],
        "not in the group: the tenant's alone"
    );
    assert_eq!(
        ids(&assignments
            .list_for_store(mine, opened_later)
            .await
            .expect("by store")),
        vec![at.assignment(3)],
        "a store nothing has named yet is reached by the tenant's"
    );

    // Each wider scope reads by what it names.
    assert_eq!(
        ids(&assignments
            .list_for_group(mine, airport)
            .await
            .expect("by group")),
        vec![at.assignment(2)]
    );
    assert!(
        assignments
            .list_for_group(mine, at.group(2))
            .await
            .expect("by group")
            .is_empty(),
        "a group nothing names"
    );
    assert_eq!(
        ids(&assignments
            .list_tenant_wide(mine)
            .await
            .expect("tenant-wide")),
        vec![at.assignment(3)]
    );
    assert_eq!(
        ids(&assignments
            .list_for_employee(mine, bao)
            .await
            .expect("by employee")),
        vec![at.assignment(2)]
    );
    assert_eq!(
        ids(&assignments
            .list_for_role(mine, cashier)
            .await
            .expect("by role")),
        vec![at.assignment(1), at.assignment(2)],
        "by role, whatever the scope"
    );

    // One per person and store, per person and group, and tenant-wide; the scopes do not exclude
    // each other. A second is refused as a duplicate, not as a failure of the store, and the first
    // keeps the role it granted.
    for (first, again) in [
        (
            &at_north,
            assign(11, alice, AssignmentScope::Store(north), lead),
        ),
        (
            &through_airport,
            assign(12, bao, AssignmentScope::StoreGroup(airport), lead),
        ),
        (
            &everywhere,
            assign(13, cam, AssignmentScope::Tenant, cashier),
        ),
    ] {
        let refused = assignments.assign(&again).await;
        assert!(
            matches!(refused, Err(AssignmentStoreError::AlreadyAssigned(_))),
            "the same person at the same place twice is a duplicate ({:?}): {refused:?}",
            again.scope
        );
        let held = assignments
            .list_for_employee(mine, again.employee_id)
            .await
            .expect("by employee");
        assert_eq!(
            held.iter()
                .map(|row| (row.assignment_id, row.role_template_id))
                .collect::<Vec<_>>(),
            vec![(first.assignment_id, first.role_template_id)],
            "the refused one wrote nothing, and the first is as it was"
        );
    }
    assignments
        .assign(&assign(4, alice, AssignmentScope::Tenant, lead))
        .await
        .expect("a store's assignment and a tenant-wide one, for the same person");
    assignments
        .assign(&assign(
            5,
            bao,
            AssignmentScope::StoreGroup(at.group(2)),
            lead,
        ))
        .await
        .expect("the same person in another group");
    assert_eq!(
        ids(&assignments
            .list_for_employee(mine, alice)
            .await
            .expect("by employee")),
        vec![at.assignment(1), at.assignment(4)],
        "both of hers, and none refused"
    );

    // The group's reach follows its membership: a store that leaves is no longer reached by it,
    // and one that joins is, from the next read on.
    set_members(groups, mine, airport, &[south, east], &version).await;
    assert_eq!(
        ids(&assignments
            .list_for_store(mine, north)
            .await
            .expect("by store")),
        vec![at.assignment(1), at.assignment(3), at.assignment(4)],
        "the store that left keeps its own and the tenant's, and loses the group's"
    );
    assert_eq!(
        ids(&assignments
            .list_for_store(mine, east)
            .await
            .expect("by store")),
        vec![at.assignment(2), at.assignment(3), at.assignment(4)],
        "the store that joined is reached by the group's"
    );

    // Nothing crosses a tenant: another tenant's reads find none of it, a store of the same id
    // included, and its id-scoped reads and removals match nothing.
    for store in [north, south, east, opened_later] {
        assert!(
            assignments
                .list_for_store(other, store)
                .await
                .expect("other tenant")
                .is_empty(),
            "another tenant's store is reached by none of these"
        );
    }
    assert!(
        assignments
            .list_tenant_wide(other)
            .await
            .expect("other tenant")
            .is_empty()
    );
    assert!(
        assignments
            .list_for_group(other, airport)
            .await
            .expect("other tenant")
            .is_empty()
    );
    assert!(
        assignments
            .get(other, at.assignment(2))
            .await
            .expect("other tenant")
            .is_none()
    );
    assert!(
        !assignments
            .remove(other, at.assignment(3))
            .await
            .expect("remove across a tenant"),
        "a removal is scoped by tenant"
    );

    // A removal takes the assignment off every store it reached, says whether there was one, and
    // leaves the rest.
    assert!(
        assignments
            .remove(mine, at.assignment(3))
            .await
            .expect("remove")
    );
    assert!(
        !assignments
            .remove(mine, at.assignment(3))
            .await
            .expect("remove again"),
        "removing what is gone removes nothing"
    );
    assert!(
        assignments
            .remove(mine, at.assignment(2))
            .await
            .expect("remove")
    );
    assert_eq!(
        ids(&assignments
            .list_for_store(mine, east)
            .await
            .expect("by store")),
        vec![at.assignment(4)],
        "the group's and the first tenant-wide one are gone; Alice's tenant-wide one stays"
    );
    assert!(
        assignments
            .list_tenant_wide(mine)
            .await
            .expect("tenant-wide")
            .iter()
            .all(|row| row.employee_id == alice),
        "only Alice's tenant-wide assignment remains"
    );
    assert!(
        assignments
            .list_for_group(mine, airport)
            .await
            .expect("by group")
            .is_empty()
    );
}
