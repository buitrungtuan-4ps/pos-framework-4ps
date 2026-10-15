// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! What each role held at a store does not grant, read before the store enforces each person's own
//! permissions ([ADR-0158](../../../docs/adr/0158-the-till-enforces-each-persons-own-permissions.md),
//! Rollout: "Before an owner turns it on, the console lists which assigned roles at that store lack
//! which permissions").
//!
//! Pure, like [`crate::people_compiler`], and over the same inputs: every assignment that reaches
//! the store, whatever its scope, and the tenant's people and roles. It compares each role with the
//! permission catalogue and nothing else. What anybody has done at the till is not an input, and
//! must not become one: guessing what a person needs from what they did would be monitoring staff
//! (ADR-0070), and a role is the owner's statement of what a job may do.
//!
//! # Which roles, and which people
//!
//! A role is listed when someone on the store's roster holds it: an active person, through an
//! assignment that reaches the store, with the role active. Those are the people the compiler puts
//! on the store's `permissions` node, and a role held only by archived people reaches nobody. An
//! archived role grants nothing, so it is not listed either; a person whose every role is archived,
//! or gone, is counted in [`StoreReadiness::people_without_role`]: with the store enforcing, they
//! can do nothing at all.
//!
//! # Which permissions
//!
//! A role misses a permission when it grants it neither directly nor with approval (decision 4).
//! Only the permissions a store decides count: the catalogue's cloud administration group is
//! never asked for at a till, so a role that lacks it lacks nothing there. They come everyday
//! permissions first — low risk, then medium, then high — and in catalogue order within a level,
//! so what stops a shift from running reads before what a role is often meant to lack.

use std::collections::{BTreeMap, BTreeSet};

use pos_core::permission::{Permission, PermissionGroup};
use serde::Serialize;

use crate::people::{Assignment, Employee, EmployeeId, RoleTemplate, RoleTemplateId};
use crate::registry::EntityStatus;

/// One catalogue permission a role grants neither directly nor with approval.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct MissingPermission {
    /// The permission's `domain.resource.action` id.
    pub id: &'static str,
    /// Its group token, such as `SALES`.
    pub group: &'static str,
    /// Its risk token: `LOW`, `MEDIUM` or `HIGH`.
    pub risk: &'static str,
}

/// One active role held at the store, and what it does not grant.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RoleReadiness {
    /// The role.
    pub role_template_id: RoleTemplateId,
    /// The role's name — a job, never a person's.
    pub name: String,
    /// How many people on the store's roster hold it, each counted once however many of their
    /// assignments name it.
    pub people: usize,
    /// What it does not grant, everyday permissions first.
    pub missing: Vec<MissingPermission>,
}

/// Every role held at one store against the catalogue, and how many people there hold none.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoreReadiness {
    /// The roles, by name and then id.
    pub roles: Vec<RoleReadiness>,
    /// How many people on the store's roster hold no active role there.
    pub people_without_role: usize,
}

/// The catalogue permissions a store decides, everyday ones first: by risk, and within a level in
/// catalogue order.
fn store_permissions() -> Vec<Permission> {
    let mut permissions: Vec<Permission> = Permission::ALL
        .iter()
        .copied()
        .filter(|permission| permission.meta().group != PermissionGroup::CloudAdministration)
        .collect();
    // Stable, so catalogue order holds within each level.
    permissions.sort_by_key(|permission| permission.meta().risk);
    permissions
}

/// Lists what each role held at a store does not grant (see the module docs).
///
/// `assignments` is every assignment that reaches the store
/// ([`AssignmentStore::list_for_store`](crate::people::AssignmentStore::list_for_store)), and
/// `employees` and `roles` the tenant's, archived ones included.
#[must_use]
pub fn store_readiness(
    employees: &[Employee],
    roles: &[RoleTemplate],
    assignments: &[Assignment],
) -> StoreReadiness {
    let on_roster: BTreeSet<EmployeeId> = employees
        .iter()
        .filter(|employee| employee.status == EntityStatus::Active)
        .map(|employee| employee.employee_id)
        .collect();
    let active: BTreeMap<RoleTemplateId, &RoleTemplate> = roles
        .iter()
        .filter(|role| role.status == EntityStatus::Active)
        .map(|role| (role.role_template_id, role))
        .collect();

    let mut at_store: BTreeSet<EmployeeId> = BTreeSet::new();
    let mut holders: BTreeMap<RoleTemplateId, BTreeSet<EmployeeId>> = BTreeMap::new();
    for assignment in assignments {
        if !on_roster.contains(&assignment.employee_id) {
            continue;
        }
        at_store.insert(assignment.employee_id);
        if active.contains_key(&assignment.role_template_id) {
            holders
                .entry(assignment.role_template_id)
                .or_default()
                .insert(assignment.employee_id);
        }
    }
    let with_role: BTreeSet<EmployeeId> = holders.values().flatten().copied().collect();

    let permissions = store_permissions();
    let mut listed: Vec<RoleReadiness> = holders
        .iter()
        .filter_map(|(role_template_id, people)| {
            let role = active.get(role_template_id)?;
            let granted = |id: &str| {
                role.permissions.iter().any(|held| held == id)
                    || role.permissions_with_approval.iter().any(|held| held == id)
            };
            let missing = permissions
                .iter()
                .map(|permission| permission.meta())
                .filter(|meta| !granted(meta.id))
                .map(|meta| MissingPermission {
                    id: meta.id,
                    group: meta.group.as_token(),
                    risk: meta.risk.as_token(),
                })
                .collect();
            Some(RoleReadiness {
                role_template_id: *role_template_id,
                name: role.name.clone(),
                people: people.len(),
                missing,
            })
        })
        .collect();
    listed.sort_by(|a, b| {
        a.name
            .cmp(&b.name)
            .then_with(|| a.role_template_id.cmp(&b.role_template_id))
    });

    StoreReadiness {
        roles: listed,
        people_without_role: at_store.difference(&with_role).count(),
    }
}

#[cfg(test)]
mod tests {
    use super::{MissingPermission, store_permissions, store_readiness};

    use pos_core::permission::{Permission, PermissionGroup, RiskLevel};
    use pos_proto::ids::{StoreId, TenantId};
    use pos_proto::ulid::Ulid;

    use crate::people::{
        Assignment, AssignmentId, AssignmentScope, Employee, EmployeeId, RoleTemplate,
        RoleTemplateId,
    };
    use crate::registry::EntityStatus;
    use crate::store_groups::StoreGroupId;

    fn tenant() -> TenantId {
        TenantId::new(Ulid::from_u128(0x7E))
    }

    fn employee(id: u128, status: EntityStatus) -> Employee {
        Employee {
            employee_id: EmployeeId::new(Ulid::from_u128(id)),
            tenant_id: tenant(),
            code: format!("C{id:02}"),
            name: format!("Person {id}"),
            status,
            has_pin: true,
        }
    }

    fn role(id: u128, name: &str, directly: &[&str], with_approval: &[&str]) -> RoleTemplate {
        RoleTemplate {
            role_template_id: RoleTemplateId::new(Ulid::from_u128(id)),
            tenant_id: tenant(),
            name: name.to_owned(),
            permissions: directly.iter().map(|id| (*id).to_owned()).collect(),
            permissions_with_approval: with_approval.iter().map(|id| (*id).to_owned()).collect(),
            discount_ceiling_minor: None,
            status: EntityStatus::Active,
        }
    }

    fn assignment(id: u128, employee: u128, scope: AssignmentScope, role: u128) -> Assignment {
        Assignment {
            assignment_id: AssignmentId::new(Ulid::from_u128(id)),
            tenant_id: tenant(),
            employee_id: EmployeeId::new(Ulid::from_u128(employee)),
            scope,
            role_template_id: RoleTemplateId::new(Ulid::from_u128(role)),
            employee_name: None,
            employee_code: None,
        }
    }

    fn at_store() -> AssignmentScope {
        AssignmentScope::Store(StoreId::new(Ulid::from_u128(0x5)))
    }

    fn ids(missing: &[MissingPermission]) -> Vec<&'static str> {
        missing.iter().map(|permission| permission.id).collect()
    }

    /// Every permission a store decides, so a role can be built that lacks exactly one of them.
    fn every_store_permission() -> Vec<&'static str> {
        store_permissions()
            .iter()
            .map(|permission| permission.meta().id)
            .collect()
    }

    /// A person reached through the store, a group and the tenant is one person, and a role held
    /// through two scopes is one role; a person with two roles is under each.
    #[test]
    fn people_and_roles_are_counted_once_across_scopes() {
        let group = AssignmentScope::StoreGroup(StoreGroupId::new(Ulid::from_u128(0x6A)));
        let employees = vec![
            employee(1, EntityStatus::Active),
            employee(2, EntityStatus::Active),
        ];
        let roles = vec![
            role(10, "Cashier", &["billing.payment.take"], &[]),
            role(11, "Lead", &[], &["billing.bill.void"]),
        ];
        let assignments = vec![
            assignment(20, 1, at_store(), 10),
            assignment(21, 1, AssignmentScope::Tenant, 10),
            assignment(22, 1, group, 11),
            assignment(23, 2, group, 10),
        ];

        let readiness = store_readiness(&employees, &roles, &assignments);

        let counts: Vec<(&str, usize)> = readiness
            .roles
            .iter()
            .map(|role| (role.name.as_str(), role.people))
            .collect();
        assert_eq!(counts, vec![("Cashier", 2), ("Lead", 1)]);
        assert_eq!(readiness.people_without_role, 0);
        // A permission granted with approval is granted: the till asks for an approver, it does not
        // refuse.
        let lead = &readiness.roles[1];
        assert!(!ids(&lead.missing).contains(&"billing.bill.void"));
        assert!(ids(&readiness.roles[0].missing).contains(&"billing.bill.void"));
    }

    /// An archived role grants nothing and is not listed; its holders with no other role are the
    /// people who could do nothing. An archived person is not on the roster at all, and a role only
    /// archived people hold is not listed.
    #[test]
    fn archived_roles_and_people_are_left_out() {
        let mut retired = role(11, "Old", &["sales.line.add"], &[]);
        retired.status = EntityStatus::Archived;
        let employees = vec![
            employee(1, EntityStatus::Active),
            employee(2, EntityStatus::Active),
            employee(3, EntityStatus::Archived),
        ];
        let roles = vec![
            role(10, "Cashier", &["billing.payment.take"], &[]),
            retired,
            role(12, "Cook", &["sales.ticket.bump"], &[]),
        ];
        let assignments = vec![
            assignment(20, 1, at_store(), 10),
            assignment(21, 1, at_store(), 11),
            // Holds only the archived role, and a role nobody has any more.
            assignment(22, 2, AssignmentScope::Tenant, 11),
            assignment(23, 2, at_store(), 999),
            // Archived: off the roster, so the role only they hold is not listed.
            assignment(24, 3, at_store(), 12),
        ];

        let readiness = store_readiness(&employees, &roles, &assignments);

        let names: Vec<&str> = readiness
            .roles
            .iter()
            .map(|role| role.name.as_str())
            .collect();
        assert_eq!(names, vec!["Cashier"]);
        assert_eq!(readiness.roles[0].people, 1);
        assert_eq!(
            readiness.people_without_role, 1,
            "the person whose roles are archived or gone"
        );
    }

    /// Everyday permissions come first: low, then medium, then high, and catalogue order within a
    /// level. The cloud's own administration is never missing at a store.
    #[test]
    fn missing_permissions_come_everyday_first_in_catalogue_order() {
        let readiness = store_readiness(
            &[employee(1, EntityStatus::Active)],
            &[role(10, "New", &[], &[])],
            &[assignment(20, 1, at_store(), 10)],
        );
        let missing = &readiness.roles[0].missing;

        let ranks: Vec<&str> = missing.iter().map(|permission| permission.risk).collect();
        let mut sorted = ranks.clone();
        sorted.sort_by_key(|risk| {
            ["LOW", "MEDIUM", "HIGH"]
                .iter()
                .position(|level| level == risk)
        });
        assert_eq!(ranks, sorted, "low, then medium, then high");
        for level in [RiskLevel::Low, RiskLevel::Medium, RiskLevel::High] {
            let in_catalogue: Vec<&str> = Permission::ALL
                .iter()
                .map(|permission| permission.meta())
                .filter(|meta| {
                    meta.risk == level && meta.group != PermissionGroup::CloudAdministration
                })
                .map(|meta| meta.id)
                .collect();
            let listed: Vec<&str> = missing
                .iter()
                .filter(|permission| permission.risk == level.as_token())
                .map(|permission| permission.id)
                .collect();
            assert_eq!(
                listed,
                in_catalogue,
                "{} in catalogue order",
                level.as_token()
            );
        }
        assert!(
            missing
                .iter()
                .all(|permission| permission.group != "CLOUD_ADMINISTRATION")
        );
        assert_eq!(
            missing.first().map(|permission| permission.risk),
            Some("LOW"),
            "an everyday permission first"
        );

        // A role that grants every permission a store decides misses nothing.
        let everything = every_store_permission();
        let complete = store_readiness(
            &[employee(1, EntityStatus::Active)],
            &[role(10, "Owner", &everything, &[])],
            &[assignment(20, 1, at_store(), 10)],
        );
        assert!(complete.roles[0].missing.is_empty());
    }
}
