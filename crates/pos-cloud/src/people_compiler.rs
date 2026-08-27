// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! Compile a store's people, roles, and assignments into the flat, edge-shaped `permissions` document
//! the store's config node carries ([ADR-0070](../../../docs/adr/0070-people-and-access.md), Track M1
//! slice 5).
//!
//! Pure: the caller loads the domain (the store's assignments, the tenant's employees and role
//! templates, and each assigned employee's stored PIN hash) and this turns it into the document the
//! edge applies (a later slice). The document rides the config tree to the store like every other
//! config change (ADR-0033) — no new channel. Per assigned, active employee it carries the `id`,
//! `code`, `name`, the granted permission set (the assignment's role flattened to its `pos-core`
//! permission ids, deduped and sorted), and the **Argon2id PIN hash** the edge verifies against offline
//! (ADR-0030) — never the PIN itself. Staff are emitted sorted by `code` so the document is stable
//! (two publishes of the same state produce byte-identical JSON).

use std::collections::BTreeMap;

use serde::Serialize;

use pos_proto::ids::StoreId;

use crate::people::{Assignment, Employee, RoleTemplate};
use crate::registry::EntityStatus;

/// One staff member as the edge reads them: identity, the flattened permission set, and the PIN hash to
/// verify against offline. The name is here because the edge shows it on screen; the hash is here
/// because the edge authenticates against it — neither is exposed back over the console API or the
/// audit trail (ADR-0070).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct StaffMember {
    /// The employee id (a ULID string).
    pub id: String,
    /// The staff/badge code the person types at the edge.
    pub code: String,
    /// The person's name, for the edge's display.
    pub name: String,
    /// The granted `pos-core` permission ids, deduped and sorted.
    pub permissions: Vec<String>,
    /// The Argon2id PHC hash of the PIN, or `None` if no PIN is set (the person cannot sign in until
    /// one is). Never the PIN itself.
    pub pin_phc: Option<String>,
}

/// The `permissions` config node for one store: the store id and its staff, in a stable order.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PermissionsDocument {
    /// The store the document authorises staff for (a ULID string).
    pub store_id: String,
    /// The store's staff, sorted by `code`.
    pub staff: Vec<StaffMember>,
}

/// Compiles a store's assignments into its `permissions` document.
///
/// `pins` maps an employee id (its ULID string) to that employee's stored PIN hash, `None` when unset.
/// Only **active** employees are emitted (an archived person is offboarded from the published set even
/// if a stale assignment lingers); an assignment whose employee is missing or archived is skipped, and
/// one whose role is missing contributes an empty permission set rather than failing the compile.
#[must_use]
pub fn compile_permissions(
    store_id: StoreId,
    employees: &[Employee],
    roles: &[RoleTemplate],
    assignments: &[Assignment],
    pins: &BTreeMap<String, Option<String>>,
) -> PermissionsDocument {
    let employee_by_id: BTreeMap<String, &Employee> = employees
        .iter()
        .map(|employee| (employee.employee_id.to_string(), employee))
        .collect();
    let role_by_id: BTreeMap<String, &RoleTemplate> = roles
        .iter()
        .map(|role| (role.role_template_id.to_string(), role))
        .collect();

    let mut staff: Vec<StaffMember> = assignments
        .iter()
        .filter_map(|assignment| {
            let employee = employee_by_id.get(&assignment.employee_id.to_string())?;
            if employee.status != EntityStatus::Active {
                return None;
            }
            let mut permissions: Vec<String> = role_by_id
                .get(&assignment.role_template_id.to_string())
                .map(|role| role.permissions.clone())
                .unwrap_or_default();
            permissions.sort_unstable();
            permissions.dedup();
            Some(StaffMember {
                id: employee.employee_id.to_string(),
                code: employee.code.clone(),
                name: employee.name.clone(),
                permissions,
                pin_phc: pins
                    .get(&employee.employee_id.to_string())
                    .cloned()
                    .flatten(),
            })
        })
        .collect();
    staff.sort_by(|a, b| a.code.cmp(&b.code));

    PermissionsDocument {
        store_id: store_id.to_string(),
        staff,
    }
}

#[cfg(test)]
mod tests {
    use super::{StaffMember, compile_permissions};

    use std::collections::BTreeMap;

    use pos_proto::ids::{StoreId, TenantId};
    use pos_proto::ulid::Ulid;

    use crate::people::{
        Assignment, AssignmentId, Employee, EmployeeId, RoleTemplate, RoleTemplateId,
    };
    use crate::registry::EntityStatus;

    fn employee(id: u128, code: &str, name: &str, status: EntityStatus) -> Employee {
        Employee {
            employee_id: EmployeeId::new(Ulid::from_u128(id)),
            tenant_id: TenantId::new(Ulid::from_u128(0x7E)),
            code: code.to_owned(),
            name: name.to_owned(),
            status,
            has_pin: true,
        }
    }

    fn role(id: u128, name: &str, permissions: &[&str]) -> RoleTemplate {
        RoleTemplate {
            role_template_id: RoleTemplateId::new(Ulid::from_u128(id)),
            tenant_id: TenantId::new(Ulid::from_u128(0x7E)),
            name: name.to_owned(),
            permissions: permissions.iter().map(|p| (*p).to_owned()).collect(),
            status: EntityStatus::Active,
        }
    }

    fn assignment(id: u128, employee: u128, store: u128, role: u128) -> Assignment {
        Assignment {
            assignment_id: AssignmentId::new(Ulid::from_u128(id)),
            tenant_id: TenantId::new(Ulid::from_u128(0x7E)),
            employee_id: EmployeeId::new(Ulid::from_u128(employee)),
            store_id: StoreId::new(Ulid::from_u128(store)),
            role_template_id: RoleTemplateId::new(Ulid::from_u128(role)),
        }
    }

    #[test]
    fn flattens_roles_carries_hash_skips_archived_and_sorts_by_code() {
        let store = StoreId::new(Ulid::from_u128(0x5702E));
        let employees = vec![
            employee(1, "C02", "Bao", EntityStatus::Active),
            employee(2, "C01", "Alice", EntityStatus::Active),
            employee(3, "C99", "Gone", EntityStatus::Archived),
        ];
        let roles = vec![
            role(
                10,
                "Cashier",
                &["billing.discount.apply", "sales.item.open"],
            ),
            role(11, "Cook", &["sales.item.mark_unavailable"]),
        ];
        let assignments = vec![
            assignment(20, 1, 0x5702E, 11), // Bao -> Cook
            assignment(21, 2, 0x5702E, 10), // Alice -> Cashier
            assignment(22, 3, 0x5702E, 10), // archived employee -> skipped
        ];
        let mut pins = BTreeMap::new();
        pins.insert(
            EmployeeId::new(Ulid::from_u128(2)).to_string(),
            Some("argon2id$phc$alice".to_owned()),
        );
        pins.insert(EmployeeId::new(Ulid::from_u128(1)).to_string(), None);

        let document = compile_permissions(store, &employees, &roles, &assignments, &pins);

        assert_eq!(document.store_id, store.to_string());
        // Sorted by code: C01 (Alice) then C02 (Bao); the archived C99 is gone.
        assert_eq!(
            document
                .staff
                .iter()
                .map(|s| s.code.as_str())
                .collect::<Vec<_>>(),
            vec!["C01", "C02"]
        );
        let alice: &StaffMember = &document.staff[0];
        assert_eq!(alice.name, "Alice");
        assert_eq!(
            alice.permissions,
            vec![
                "billing.discount.apply".to_owned(),
                "sales.item.open".to_owned()
            ],
            "the role is flattened to its permissions, sorted"
        );
        assert_eq!(
            alice.pin_phc,
            Some("argon2id$phc$alice".to_owned()),
            "the PIN hash rides to the store so the edge can verify offline"
        );
        assert_eq!(document.staff[1].pin_phc, None, "no PIN set for Bao");
    }

    #[test]
    fn a_missing_role_yields_an_empty_permission_set_not_a_failure() {
        let store = StoreId::new(Ulid::from_u128(0x5702F));
        let employees = vec![employee(1, "C01", "Alice", EntityStatus::Active)];
        let roles: Vec<RoleTemplate> = vec![];
        let assignments = vec![assignment(20, 1, 0x5702F, 999)];
        let pins = BTreeMap::new();

        let document = compile_permissions(store, &employees, &roles, &assignments, &pins);
        assert_eq!(document.staff.len(), 1);
        assert!(document.staff[0].permissions.is_empty());
    }
}
