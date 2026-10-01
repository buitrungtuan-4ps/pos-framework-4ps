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
//! `code`, `name`, the permissions they hold directly (every active role they hold at the store,
//! flattened to its `pos-core` permission ids, unioned, deduped and sorted), those they hold only
//! with approval (the same, less anything held directly), the highest of those roles' discount
//! ceilings, and the **Argon2id PIN hash** the edge verifies against offline (ADR-0030) — never the
//! PIN itself. Staff are emitted sorted by `code` so the document is stable (two publishes of the
//! same state produce byte-identical JSON).

use std::collections::{BTreeMap, BTreeSet};

use pos_proto::ids::StoreId;
use pos_proto::people::{PublishedPermissions, PublishedStaffMember};

use crate::people::{Assignment, Employee, RoleTemplate};
use crate::registry::EntityStatus;

/// What one person holds at the store, gathered over every assignment they have there before
/// anything is emitted.
struct Held<'a> {
    employee: &'a Employee,
    permissions: BTreeSet<String>,
    permissions_with_approval: BTreeSet<String>,
    discount_ceiling_minor: Option<i64>,
}

/// Compiles a store's assignments into its `permissions` document.
///
/// `pins` maps an employee id (its ULID string) to that employee's stored PIN hash, `None` when unset.
/// Only **active** employees are emitted (an archived person is offboarded from the published set even
/// if a stale assignment lingers); an assignment whose employee is missing or archived is skipped.
///
/// # A person, not an assignment
///
/// Each person appears once, however many assignments they have at the store, holding the **union**
/// of their roles' permissions and the **highest** of their roles' discount ceilings
/// ([ADR-0158](../../../docs/adr/0158-the-till-enforces-each-persons-own-permissions.md) decision 3).
/// The schema allows one assignment per person and store today, and assignments that reach a store
/// group or every store will give one person several at once; a node listing the same code twice
/// would leave the till to pick one of them.
///
/// A role grants each permission directly or with approval (decision 4), and a person holds a
/// permission with approval when one of their roles grants it so and **none grants it directly**:
/// of two roles that disagree, the one that lets them act alone wins, so the two lists never
/// overlap. The edge reads them the same way.
///
/// **An archived role contributes nothing** (decision 7): no permission, either way, and no
/// ceiling, exactly as a role that is missing altogether. The person stays on the roster through the assignment,
/// holding whatever their other roles grant, and nothing if they have none; it is archiving the
/// person or removing the assignment that takes them off it.
///
/// Of two ceilings the higher wins, and a configured `0` is higher than none: both mean "every
/// discount needs a manager", but the node keeps them apart so the console can show which was said.
#[must_use]
pub fn compile_permissions(
    store_id: StoreId,
    employees: &[Employee],
    roles: &[RoleTemplate],
    assignments: &[Assignment],
    pins: &BTreeMap<String, Option<String>>,
) -> PublishedPermissions {
    let employee_by_id: BTreeMap<String, &Employee> = employees
        .iter()
        .map(|employee| (employee.employee_id.to_string(), employee))
        .collect();
    // Only a role that still grants anything is looked up, so an archived one and a missing one are
    // the same miss below and cannot drift into different answers.
    let active_role_by_id: BTreeMap<String, &RoleTemplate> = roles
        .iter()
        .filter(|role| role.status == EntityStatus::Active)
        .map(|role| (role.role_template_id.to_string(), role))
        .collect();

    let mut held: BTreeMap<String, Held<'_>> = BTreeMap::new();
    for assignment in assignments {
        let employee_id = assignment.employee_id.to_string();
        let Some(&employee) = employee_by_id.get(&employee_id) else {
            continue;
        };
        if employee.status != EntityStatus::Active {
            continue;
        }
        let person = held.entry(employee_id).or_insert_with(|| Held {
            employee,
            permissions: BTreeSet::new(),
            permissions_with_approval: BTreeSet::new(),
            discount_ceiling_minor: None,
        });
        if let Some(role) = active_role_by_id.get(&assignment.role_template_id.to_string()) {
            person.permissions.extend(role.permissions.iter().cloned());
            person
                .permissions_with_approval
                .extend(role.permissions_with_approval.iter().cloned());
            // Resolved from the role, like the permissions beside it and for the same reason: the
            // edge authorises one person at a time and should not have to hold the role table to do
            // it. `Option`'s order puts `None` below every ceiling, so `max` keeps the highest.
            person.discount_ceiling_minor = person
                .discount_ceiling_minor
                .max(role.discount_ceiling_minor);
        }
    }

    let mut staff: Vec<PublishedStaffMember> = held
        .into_values()
        .map(|person| {
            // Held directly wins (decision 4). Empty is left off the wire, so a person whose roles
            // grant nothing with approval is published exactly as before the list existed.
            let permissions_with_approval = person
                .permissions_with_approval
                .difference(&person.permissions)
                .cloned()
                .collect();
            PublishedStaffMember {
                id: Some(person.employee.employee_id.to_string()),
                code: person.employee.code.clone(),
                name: person.employee.name.clone(),
                permissions: person.permissions.into_iter().collect(),
                permissions_with_approval,
                discount_ceiling_minor: person.discount_ceiling_minor,
                pin_phc: pins
                    .get(&person.employee.employee_id.to_string())
                    .cloned()
                    .flatten(),
            }
        })
        .collect();
    // By code, which is unique within a tenant, and then by id, so the order is total by
    // construction rather than by a constraint this function cannot see.
    staff.sort_by(|a, b| a.code.cmp(&b.code).then_with(|| a.id.cmp(&b.id)));

    PublishedPermissions {
        store_id: Some(store_id.to_string()),
        // Not the compiler's to say. `permissions.enforced` is a setting the cloud resolves per
        // store and writes on the store's Tenant layer (ADR-0160), where the edge reads it merged
        // with this Store-layer node. `false` is skipped from the wire, so this node never
        // overrides it.
        enforced: false,
        staff,
    }
}

#[cfg(test)]
mod tests {
    use super::compile_permissions;
    use pos_proto::people::PublishedStaffMember;

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
        role_with_ceiling(id, name, permissions, None)
    }

    fn role_with_ceiling(
        id: u128,
        name: &str,
        permissions: &[&str],
        discount_ceiling_minor: Option<i64>,
    ) -> RoleTemplate {
        RoleTemplate {
            role_template_id: RoleTemplateId::new(Ulid::from_u128(id)),
            tenant_id: TenantId::new(Ulid::from_u128(0x7E)),
            name: name.to_owned(),
            permissions: permissions.iter().map(|p| (*p).to_owned()).collect(),
            permissions_with_approval: Vec::new(),
            discount_ceiling_minor,
            status: EntityStatus::Active,
        }
    }

    /// A role granting `directly` directly and `with_approval` only with somebody's approval.
    fn role_with_approval(
        id: u128,
        name: &str,
        directly: &[&str],
        with_approval: &[&str],
    ) -> RoleTemplate {
        let mut role = role(id, name, directly);
        role.permissions_with_approval = with_approval.iter().map(|p| (*p).to_owned()).collect();
        role
    }

    fn assignment(id: u128, employee: u128, store: u128, role: u128) -> Assignment {
        Assignment {
            assignment_id: AssignmentId::new(Ulid::from_u128(id)),
            tenant_id: TenantId::new(Ulid::from_u128(0x7E)),
            employee_id: EmployeeId::new(Ulid::from_u128(employee)),
            store_id: StoreId::new(Ulid::from_u128(store)),
            role_template_id: RoleTemplateId::new(Ulid::from_u128(role)),
            // The compiler pairs an assignment with its employee by id, out of the whole roster it
            // is handed — the resolved name is for the console's paged read, and this path has no
            // use for it.
            employee_name: None,
            employee_code: None,
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

        assert_eq!(document.store_id, Some(store.to_string()));
        // Sorted by code: C01 (Alice) then C02 (Bao); the archived C99 is gone.
        assert_eq!(
            document
                .staff
                .iter()
                .map(|s| s.code.as_str())
                .collect::<Vec<_>>(),
            vec!["C01", "C02"]
        );
        let alice: &PublishedStaffMember = &document.staff[0];
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
        assert_eq!(
            document.staff[0].discount_ceiling_minor, None,
            "a person whose role is missing gets no allowance either"
        );
    }

    /// The role's ceiling reaches the person, which is the whole of what the node had to carry for a
    /// server to discount without a manager.
    #[test]
    fn a_role_carries_its_discount_ceiling_onto_the_people_assigned_to_it() {
        let store = StoreId::new(Ulid::from_u128(0x5702F));
        let employees = vec![
            employee(1, "C01", "Alice", EntityStatus::Active),
            employee(2, "C02", "Bao", EntityStatus::Active),
        ];
        let roles = vec![
            role_with_ceiling(10, "Server", &["billing.discount.apply"], Some(30_000)),
            // Deliberately a *zero* rather than an absent one: the two are different statements and
            // the node has to keep them apart, or a console could not show which was said.
            role_with_ceiling(11, "Trainee", &["billing.discount.apply"], Some(0)),
        ];
        let assignments = vec![
            assignment(20, 1, 0x5702F, 10),
            assignment(21, 2, 0x5702F, 11),
        ];
        let pins = BTreeMap::new();

        let document = compile_permissions(store, &employees, &roles, &assignments, &pins);
        assert_eq!(document.staff[0].discount_ceiling_minor, Some(30_000));
        assert_eq!(document.staff[1].discount_ceiling_minor, Some(0));

        // And an absent ceiling is left off the wire entirely, so a tenant that configures none
        // publishes the document it published before the field existed.
        let plain = compile_permissions(
            store,
            &employees[..1],
            &[role(10, "Server", &["billing.discount.apply"])],
            &assignments[..1],
            &pins,
        );
        let json = serde_json::to_value(&plain).expect("serialise");
        assert!(
            json["staff"][0].get("discount_ceiling_minor").is_none(),
            "an absent ceiling should not appear on the wire: {json}"
        );
    }

    fn archived(mut role: RoleTemplate) -> RoleTemplate {
        role.status = EntityStatus::Archived;
        role
    }

    /// ADR-0158 decision 7: a role an owner has retired stops granting when it is archived, instead
    /// of going on granting in every node published after it.
    #[test]
    fn an_archived_role_grants_nothing() {
        let store = StoreId::new(Ulid::from_u128(0x5703A));
        let employees = vec![
            employee(1, "C01", "Alice", EntityStatus::Active),
            employee(2, "C02", "Bao", EntityStatus::Active),
        ];
        let roles = vec![
            archived(role_with_ceiling(
                10,
                "Supervisor",
                &["billing.discount.override_ceiling", "sales.line.void_fired"],
                Some(500_000),
            )),
            role_with_ceiling(11, "Server", &["sales.line.add"], Some(20_000)),
        ];
        let assignments = vec![
            // Alice holds only the archived role; Bao holds it and an active one.
            assignment(20, 1, 0x5703A, 10),
            assignment(21, 2, 0x5703A, 10),
            assignment(22, 2, 0x5703A, 11),
        ];

        let document =
            compile_permissions(store, &employees, &roles, &assignments, &BTreeMap::new());

        let alice = &document.staff[0];
        assert_eq!(
            alice.code, "C01",
            "still on the roster through her assignment"
        );
        assert!(
            alice.permissions.is_empty() && alice.discount_ceiling_minor.is_none(),
            "an archived role grants no permission and no ceiling: {alice:?}"
        );
        let bao = &document.staff[1];
        assert_eq!(
            bao.permissions,
            vec!["sales.line.add".to_owned()],
            "only the active role counts"
        );
        assert_eq!(bao.discount_ceiling_minor, Some(20_000));
    }

    /// ADR-0158 decision 3: a person with several roles at one store holds all of them at once.
    #[test]
    fn several_assignments_at_one_store_give_the_union_and_the_highest_ceiling() {
        let store = StoreId::new(Ulid::from_u128(0x5703B));
        let employees = vec![employee(1, "C01", "Alice", EntityStatus::Active)];
        let roles = vec![
            role_with_ceiling(
                10,
                "Cashier",
                &["billing.discount.apply", "billing.payment.take"],
                Some(10_000),
            ),
            role_with_ceiling(
                11,
                "Shift lead",
                &["billing.payment.take", "cash.shift.open"],
                Some(50_000),
            ),
            // No ceiling at all is the lowest: it cannot pull the highest one down.
            role(12, "Runner", &["sales.ticket.bump"]),
        ];
        let assignments = vec![
            assignment(20, 1, 0x5703B, 10),
            assignment(21, 1, 0x5703B, 11),
            assignment(22, 1, 0x5703B, 12),
        ];

        let document =
            compile_permissions(store, &employees, &roles, &assignments, &BTreeMap::new());

        assert_eq!(document.staff.len(), 1, "one person, listed once");
        let alice = &document.staff[0];
        assert_eq!(
            alice.permissions,
            vec![
                "billing.discount.apply".to_owned(),
                "billing.payment.take".to_owned(),
                "cash.shift.open".to_owned(),
                "sales.ticket.bump".to_owned(),
            ],
            "the union, sorted and without the shared permission twice"
        );
        assert_eq!(alice.discount_ceiling_minor, Some(50_000));

        // A configured zero still beats no ceiling: both need a manager, and the node says which.
        let zero_and_none = compile_permissions(
            store,
            &employees,
            &[
                role_with_ceiling(10, "Trainee", &[], Some(0)),
                role(11, "Runner", &[]),
            ],
            &assignments[..2],
            &BTreeMap::new(),
        );
        assert_eq!(zero_and_none.staff[0].discount_ceiling_minor, Some(0));
    }

    /// Two compiles of the same people produce the same bytes, whatever order the store hands the
    /// assignments back in: a store reloads its tills on every new version, so a node that
    /// reshuffled itself would be a reload for nothing.
    #[test]
    fn the_document_is_byte_stable_whatever_order_the_assignments_arrive_in() {
        let store = StoreId::new(Ulid::from_u128(0x5703C));
        let employees = vec![
            employee(1, "C02", "Bao", EntityStatus::Active),
            employee(2, "C01", "Alice", EntityStatus::Active),
        ];
        let roles = vec![
            role_with_ceiling(10, "Cashier", &["billing.payment.take"], Some(10_000)),
            role_with_ceiling(11, "Cook", &["sales.ticket.bump"], Some(0)),
        ];
        let forwards = vec![
            assignment(20, 1, 0x5703C, 10),
            assignment(21, 1, 0x5703C, 11),
            assignment(22, 2, 0x5703C, 11),
        ];
        let backwards: Vec<Assignment> = forwards.iter().rev().cloned().collect();
        let mut pins = BTreeMap::new();
        pins.insert(
            EmployeeId::new(Ulid::from_u128(2)).to_string(),
            Some("argon2id$phc$alice".to_owned()),
        );

        let one = serde_json::to_string(&compile_permissions(
            store, &employees, &roles, &forwards, &pins,
        ))
        .expect("serialise");
        let other = serde_json::to_string(&compile_permissions(
            store, &employees, &roles, &backwards, &pins,
        ))
        .expect("serialise");
        assert_eq!(one, other);
        assert!(
            one.find("\"C01\"") < one.find("\"C02\""),
            "sorted by code: {one}"
        );
    }

    /// ADR-0158 decision 4: what a person's roles grant with approval reaches the node beside what
    /// they grant directly, and a permission one role grants directly is not also listed with
    /// approval because another grants it so.
    #[test]
    fn a_role_grants_with_approval_what_it_says_and_held_directly_wins() {
        let store = StoreId::new(Ulid::from_u128(0x5703D));
        let employees = vec![
            employee(1, "C01", "Alice", EntityStatus::Active),
            employee(2, "C02", "Bao", EntityStatus::Active),
            employee(3, "C03", "Cam", EntityStatus::Active),
        ];
        let roles = vec![
            role_with_approval(
                10,
                "Server",
                &["sales.line.add"],
                &["sales.line.void_fired", "billing.bill.void"],
            ),
            role_with_approval(
                11,
                "Supervisor",
                &["sales.line.void_fired"],
                &["billing.bill.void"],
            ),
            archived(role_with_approval(
                12,
                "Old shift lead",
                &[],
                &["cash.drawer.open_no_sale"],
            )),
            role(13, "Cook", &["sales.ticket.bump"]),
        ];
        let assignments = vec![
            assignment(20, 1, 0x5703D, 10), // Alice: Server
            assignment(21, 2, 0x5703D, 10), // Bao: Server and Supervisor
            assignment(22, 2, 0x5703D, 11),
            assignment(23, 3, 0x5703D, 12), // Cam: the archived role and Cook
            assignment(24, 3, 0x5703D, 13),
        ];

        let document =
            compile_permissions(store, &employees, &roles, &assignments, &BTreeMap::new());

        let alice = &document.staff[0];
        assert_eq!(alice.permissions, vec!["sales.line.add".to_owned()]);
        assert_eq!(
            alice.permissions_with_approval,
            vec![
                "billing.bill.void".to_owned(),
                "sales.line.void_fired".to_owned()
            ],
            "what her role grants with approval, sorted"
        );
        let bao = &document.staff[1];
        assert_eq!(
            bao.permissions,
            vec![
                "sales.line.add".to_owned(),
                "sales.line.void_fired".to_owned()
            ]
        );
        assert_eq!(
            bao.permissions_with_approval,
            vec!["billing.bill.void".to_owned()],
            "once, and not the void his other role grants him directly"
        );
        let cam = &document.staff[2];
        assert!(
            cam.permissions_with_approval.is_empty(),
            "an archived role grants nothing with approval either: {cam:?}"
        );
        let json = serde_json::to_value(&document).expect("serialise");
        assert!(
            json["staff"][2].get("permissions_with_approval").is_none(),
            "an empty list is left off the wire, as before it existed: {json}"
        );
    }
}
