// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! Role templates + per-store assignments: people & access (ADR-0070, M1 slice 2).

use super::{block_on, prepared};
use pos_proto::error::ErrorStatus;
use store_postgres::RowUpdate;

/// Role templates insert with two jsonb permission sets — what the role grants directly and what
/// it grants with approval — that round-trip as their JSON text, read back tenant-scoped and
/// newest-first, and update name/permissions/status. The grant is SELECT/INSERT/UPDATE only — no
/// DELETE (a role is archived, never removed) ([ADR-0070]).
#[test]
#[expect(
    clippy::too_many_lines,
    reason = "one round trip over both permission lists, the ceiling, the update and the grant, \
              checked against the same rows"
)]
fn role_templates_round_trip_permissions_and_grant() {
    block_on(async {
        let (store, admin) = prepared().await.expect("prepare the database");
        let people = store.people();

        people
            .insert_role_template(
                "01ROLE000000000000000000A1",
                "tenant-a",
                "Cashier",
                r#"["billing.discount.apply","sales.item.open"]"#,
                // Voids a bill with somebody else's code and PIN (ADR-0158 decision 4).
                r#"["billing.bill.void"]"#,
                // A cashier who may discount up to 30,000 without a manager — the column this
                // row exists to round-trip.
                Some(30_000),
            )
            .await
            .expect("insert cashier");
        people
            .insert_role_template(
                "01ROLE000000000000000000B1",
                "tenant-b",
                "Cashier",
                "[]",
                "[]",
                None,
            )
            .await
            .expect("a duplicate name is fine under a different tenant");

        // The unique index refuses a second "Cashier" within tenant-a.
        assert!(
            people
                .insert_role_template(
                    "01ROLE000000000000000000A2",
                    "tenant-a",
                    "Cashier",
                    "[]",
                    "[]",
                    None
                )
                .await
                .is_err(),
            "role names are unique within a tenant"
        );

        // Tenant-scoped read; each jsonb permission set round-trips as its JSON text.
        let scoped = people
            .fetch_role_templates("tenant-a")
            .await
            .expect("fetch tenant-a");
        assert_eq!(scoped.len(), 1, "only tenant-a's own roles");
        let cashier = scoped.first().expect("a row");
        let permissions: Vec<String> =
            serde_json::from_str(&cashier.permissions_json).expect("permissions are JSON");
        assert_eq!(
            permissions,
            vec![
                "billing.discount.apply".to_owned(),
                "sales.item.open".to_owned()
            ]
        );
        let with_approval: Vec<String> =
            serde_json::from_str(&cashier.permissions_with_approval_json)
                .expect("permissions with approval are JSON");
        assert_eq!(
            with_approval,
            vec!["billing.bill.void".to_owned()],
            "what the role grants with approval is its own column, apart from the direct list"
        );
        assert_eq!(
            cashier.discount_ceiling_minor,
            Some(30_000),
            "the ceiling round-trips through the column"
        );

        // Update the permission sets + archive, at the version the read handed out (ADR-0094).
        assert!(
            matches!(
                people
                    .set_role_template(
                        "tenant-a",
                        "01ROLE000000000000000000A1",
                        "Cashier",
                        r#"["sales.item.open"]"#,
                        r#"["billing.bill.void","sales.line.void_fired"]"#,
                        "archived",
                        None,
                        &cashier.version,
                    )
                    .await
                    .expect("update"),
                RowUpdate::Updated(_)
            ),
            "the row changed"
        );
        let updated = people
            .fetch_role_template("tenant-a", "01ROLE000000000000000000A1")
            .await
            .expect("fetch one")
            .expect("present");
        assert_eq!(updated.status, "archived");
        let updated_permissions: Vec<String> =
            serde_json::from_str(&updated.permissions_json).expect("permissions are JSON");
        assert_eq!(updated_permissions, vec!["sales.item.open".to_owned()]);
        let updated_with_approval: Vec<String> =
            serde_json::from_str(&updated.permissions_with_approval_json)
                .expect("permissions with approval are JSON");
        assert_eq!(
            updated_with_approval,
            vec![
                "billing.bill.void".to_owned(),
                "sales.line.void_fired".to_owned()
            ]
        );

        // Roles are archived, never deleted: no DELETE grant.
        let can_delete: bool = admin
            .query_one(
                "SELECT has_table_privilege('app_tenant', 'role_templates', 'DELETE')",
                &[],
            )
            .await
            .expect("privilege check")
            .get(0);
        assert!(
            !can_delete,
            "role_templates is never DELETEd — archived instead"
        );
    });
}

/// Every role that exists gains the seven till permissions once (migration 0073, ADR-0158), and a
/// later boot does not hand back one an owner has since removed.
///
/// `prepared` truncates `data_migrations`, so the boot below is the grant's first run, as it is
/// on a database that has never seen the file.
#[test]
fn existing_roles_keep_every_till_action_and_a_removal_sticks() {
    const ROLE: &str = "01ROLE000000000000000000C1";
    const TILL: [&str; 7] = [
        "billing.bill.open",
        "billing.bill.split",
        "billing.payment.take",
        "sales.line.add",
        "sales.line.fire",
        "sales.table.manage",
        "sales.ticket.bump",
    ];
    block_on(async {
        let (store, _admin) = prepared().await.expect("prepare the database");
        let people = store.people();
        people
            .insert_role_template(
                ROLE,
                "tenant-a",
                "Server",
                r#"["billing.discount.apply","sales.line.add"]"#,
                "[]",
                None,
            )
            .await
            .expect("a role authored before the seven had names");

        store
            .migrate()
            .await
            .expect("the boot that runs 0073 first");
        let role = people
            .fetch_role_template("tenant-a", ROLE)
            .await
            .expect("fetch")
            .expect("present");
        let granted: Vec<String> =
            serde_json::from_str(&role.permissions_json).expect("permissions are JSON");
        let mut expected: Vec<String> = TILL.iter().map(|id| (*id).to_owned()).collect();
        expected.push("billing.discount.apply".to_owned());
        expected.sort();
        assert_eq!(
            granted, expected,
            "all seven, what it had, sorted, and sales.line.add once"
        );

        // The owner takes payment off servers on purpose.
        let without_payment: Vec<&str> = expected
            .iter()
            .map(String::as_str)
            .filter(|id| *id != "billing.payment.take")
            .collect();
        let written = serde_json::to_string(&without_payment).expect("serialise");
        people
            .set_role_template(
                "tenant-a",
                ROLE,
                "Server",
                &written,
                &role.permissions_with_approval_json,
                "active",
                None,
                &role.version,
            )
            .await
            .expect("the owner edits the role");

        store.migrate().await.expect("the next boot");
        let after = people
            .fetch_role_template("tenant-a", ROLE)
            .await
            .expect("fetch")
            .expect("present");
        let kept: Vec<String> =
            serde_json::from_str(&after.permissions_json).expect("permissions are JSON");
        assert!(
            !kept.iter().any(|id| id == "billing.payment.take"),
            "a later boot does not hand back what the owner removed"
        );
        assert_eq!(kept.len(), without_payment.len());
    });
}

/// Every role that exists is given, once, each PIN-flagged permission it does not grant
/// directly, with approval (migration 0074, ADR-0158 decision 4). What it grants directly is
/// left byte for byte as it was, and a later boot does not hand back an approval an owner has
/// since taken away.
///
/// The 0073, 0076, 0079, 0084, 0086 and 0087 markers are written before the boot, so the boot runs
/// 0074 alone against the roles as they were authored.
#[test]
#[expect(
    clippy::too_many_lines,
    reason = "one boot over three roles and the boot after it, checked against the same rows"
)]
fn existing_roles_grant_each_pin_flagged_permission_with_approval_once() {
    const FLAGGED: [&str; 7] = [
        "billing.bill.void",
        "billing.comp.apply",
        "billing.discount.override_ceiling",
        "billing.price.override",
        "billing.refund.issue",
        "cash.drawer.open_no_sale",
        "sales.line.void_fired",
    ];
    const MANAGER: &str = "01ROLE000000000000000000M1";
    const SERVER: &str = "01ROLE000000000000000000S1";
    const RETIRED: &str = "01ROLE000000000000000000R1";
    block_on(async {
        let (store, admin) = prepared().await.expect("prepare the database");
        admin
            .execute(
                "INSERT INTO data_migrations (name) \
                 VALUES ('0073_roles_keep_every_till_action'), ('0076_roles_can_waive_a_fee'), \
                        ('0079_approvers_who_close_a_shift_see_takings'), \
                        ('0084_roles_can_manage_another_tills_drawer'), \
                        ('0086_roles_can_override_an_assigned_drawer'), \
                        ('0087_roles_can_void_a_paid_bill')",
                &[],
            )
            .await
            .expect("0073, 0076, 0079, 0084, 0086 and 0087 have already run");
        let people = store.people();
        // A manager who voids directly, and so approves voids for others, listed out of order
        // as a console may have written it.
        people
            .insert_role_template(
                MANAGER,
                "tenant-a",
                "Manager",
                r#"["sales.line.void_fired","billing.bill.void","billing.discount.apply"]"#,
                "[]",
                Some(500_000),
            )
            .await
            .expect("a manager");
        people
            .insert_role_template(
                SERVER,
                "tenant-a",
                "Server",
                r#"["billing.discount.apply","sales.line.add"]"#,
                "[]",
                None,
            )
            .await
            .expect("a server");
        let retired = people
            .insert_role_template(
                RETIRED,
                "tenant-a",
                "Runner",
                r#"["sales.ticket.bump"]"#,
                "[]",
                None,
            )
            .await
            .expect("a role since retired");
        people
            .set_role_template(
                "tenant-a",
                RETIRED,
                "Runner",
                r#"["sales.ticket.bump"]"#,
                "[]",
                "archived",
                None,
                &retired,
            )
            .await
            .expect("archive it");
        let mut direct_before: Vec<(String, String)> = people
            .fetch_role_templates("tenant-a")
            .await
            .expect("fetch")
            .into_iter()
            .map(|role| (role.id, role.permissions_json))
            .collect();
        direct_before.sort();

        store
            .migrate()
            .await
            .expect("the boot that runs 0074 first");
        let roles = people
            .fetch_role_templates("tenant-a")
            .await
            .expect("fetch");
        let mut direct_after: Vec<(String, String)> = roles
            .iter()
            .map(|role| (role.id.clone(), role.permissions_json.clone()))
            .collect();
        direct_after.sort();
        assert_eq!(
            direct_after, direct_before,
            "what each role grants directly is byte-identical"
        );
        let with_approval = |id: &str| -> Vec<String> {
            let role = roles
                .iter()
                .find(|role| role.id == id)
                .expect("the role is listed");
            serde_json::from_str(&role.permissions_with_approval_json)
                .expect("permissions with approval are JSON")
        };
        let flagged_except = |held: &[&str]| -> Vec<String> {
            FLAGGED
                .iter()
                .filter(|id| !held.contains(id))
                .map(|id| (*id).to_owned())
                .collect()
        };
        assert_eq!(
            with_approval(MANAGER),
            flagged_except(&["billing.bill.void", "sales.line.void_fired"]),
            "the five it does not grant directly, sorted; the two it does stay direct"
        );
        assert_eq!(with_approval(SERVER), flagged_except(&[]), "all seven");
        assert_eq!(
            with_approval(RETIRED),
            flagged_except(&[]),
            "an archived role too, so restoring it restores what it had"
        );

        // The owner takes the drawer without a sale off servers altogether.
        let server = roles
            .iter()
            .find(|role| role.id == SERVER)
            .expect("the server");
        let without_drawer = flagged_except(&["cash.drawer.open_no_sale"]);
        people
            .set_role_template(
                "tenant-a",
                SERVER,
                "Server",
                &server.permissions_json,
                &serde_json::to_string(&without_drawer).expect("serialise"),
                "active",
                None,
                &server.version,
            )
            .await
            .expect("the owner edits the role");

        store.migrate().await.expect("the next boot");
        let after = people
            .fetch_role_template("tenant-a", SERVER)
            .await
            .expect("fetch")
            .expect("present");
        let kept: Vec<String> = serde_json::from_str(&after.permissions_with_approval_json)
            .expect("permissions with approval are JSON");
        assert_eq!(
            kept, without_drawer,
            "a later boot does not hand back an approval the owner took away"
        );
    });
}

/// Every role that exists is given, once, `billing.fee.waive` on the terms it grants
/// `billing.discount.override_ceiling` (migration 0076, ADR-0159 decision 5): directly where it
/// grants the override directly, and with approval otherwise. Nothing else in either list
/// changes, and a later boot does not hand back a waive an owner has since taken away.
///
/// The 0073, 0074, 0079, 0084, 0086 and 0087 markers are written before the boot, so the boot runs
/// 0076 alone against the roles as they were authored.
#[test]
#[expect(
    clippy::too_many_lines,
    reason = "one boot over four roles and the boot after it, checked against the same rows"
)]
fn existing_roles_waive_a_fee_on_the_terms_they_exceed_the_ceiling_once() {
    const MANAGER: &str = "01ROLE000000000000000000M2";
    const SUPERVISOR: &str = "01ROLE000000000000000000V2";
    const SERVER: &str = "01ROLE000000000000000000S2";
    const RETIRED: &str = "01ROLE000000000000000000R2";
    const WAIVE: &str = "billing.fee.waive";
    block_on(async {
        let (store, admin) = prepared().await.expect("prepare the database");
        admin
            .execute(
                "INSERT INTO data_migrations (name) \
                 VALUES ('0073_roles_keep_every_till_action'), \
                        ('0074_role_permissions_with_approval'), \
                        ('0079_approvers_who_close_a_shift_see_takings'), \
                        ('0084_roles_can_manage_another_tills_drawer'), \
                        ('0086_roles_can_override_an_assigned_drawer'), \
                        ('0087_roles_can_void_a_paid_bill')",
                &[],
            )
            .await
            .expect("0073, 0074, 0079, 0084, 0086 and 0087 have already run");
        let people = store.people();
        // A manager who exceeds the ceiling directly, and so approves it for others.
        people
            .insert_role_template(
                MANAGER,
                "tenant-a",
                "Manager",
                r#"["billing.bill.void","billing.discount.apply","billing.discount.override_ceiling"]"#,
                r#"["billing.comp.apply"]"#,
                Some(500_000),
            )
            .await
            .expect("a manager");
        // A supervisor who exceeds it only with somebody's approval.
        people
            .insert_role_template(
                SUPERVISOR,
                "tenant-a",
                "Supervisor",
                r#"["billing.discount.apply"]"#,
                r#"["billing.bill.void","billing.discount.override_ceiling"]"#,
                None,
            )
            .await
            .expect("a supervisor");
        // A server who cannot exceed it at all, listed out of order as a console may have
        // written it.
        people
            .insert_role_template(
                SERVER,
                "tenant-a",
                "Server",
                r#"["sales.line.add","billing.discount.apply"]"#,
                "[]",
                None,
            )
            .await
            .expect("a server");
        let retired = people
            .insert_role_template(
                RETIRED,
                "tenant-a",
                "Runner",
                r#"["sales.ticket.bump"]"#,
                "[]",
                None,
            )
            .await
            .expect("a role since retired");
        people
            .set_role_template(
                "tenant-a",
                RETIRED,
                "Runner",
                r#"["sales.ticket.bump"]"#,
                "[]",
                "archived",
                None,
                &retired,
            )
            .await
            .expect("archive it");
        let before = people
            .fetch_role_templates("tenant-a")
            .await
            .expect("fetch");
        let direct_before = |id: &str| -> String {
            before
                .iter()
                .find(|role| role.id == id)
                .map(|role| role.permissions_json.clone())
                .expect("the role is listed")
        };

        store
            .migrate()
            .await
            .expect("the boot that runs 0076 first");
        let roles = people
            .fetch_role_templates("tenant-a")
            .await
            .expect("fetch");
        let role = |id: &str| {
            roles
                .iter()
                .find(|role| role.id == id)
                .expect("the role is listed")
        };
        let list = |json: &str| -> Vec<String> {
            serde_json::from_str(json).expect("a permission list is JSON")
        };

        assert_eq!(
            list(&role(MANAGER).permissions_json),
            [
                "billing.bill.void",
                "billing.discount.apply",
                "billing.discount.override_ceiling",
                WAIVE,
            ],
            "directly, beside the override, in byte order"
        );
        assert_eq!(
            list(&role(MANAGER).permissions_with_approval_json),
            ["billing.comp.apply"],
            "not with approval as well"
        );
        for (id, approved) in [
            (
                SUPERVISOR,
                vec![
                    "billing.bill.void",
                    "billing.discount.override_ceiling",
                    WAIVE,
                ],
            ),
            (SERVER, vec![WAIVE]),
            (RETIRED, vec![WAIVE]),
        ] {
            assert_eq!(
                role(id).permissions_json,
                direct_before(id),
                "{id}: what it grants directly is byte-identical"
            );
            assert_eq!(
                list(&role(id).permissions_with_approval_json),
                approved,
                "{id}: with approval, in byte order"
            );
        }

        // The owner takes the waive off servers altogether.
        let server = role(SERVER);
        people
            .set_role_template(
                "tenant-a",
                SERVER,
                "Server",
                &server.permissions_json,
                "[]",
                "active",
                None,
                &server.version,
            )
            .await
            .expect("the owner edits the role");

        store.migrate().await.expect("the next boot");
        let after = people
            .fetch_role_template("tenant-a", SERVER)
            .await
            .expect("fetch")
            .expect("present");
        assert_eq!(
            list(&after.permissions_with_approval_json),
            Vec::<String>::new(),
            "a later boot does not hand back a waive the owner took away"
        );
    });
}

/// Every role that exists, grants `cash.shift.close` directly and grants at least one PIN-flagged
/// permission directly is given, once, `reports.takings.view` directly (migration 0079, ADR-0160
/// decision 2): an approver who closes shifts, as a new tenant's supervisor, manager and owner
/// are. No other role is, each case on a role of its own: one that closes but approves nothing
/// directly, as a cashier's does after 0074; an approver who closes no shift; and one that closes
/// only with approval. No with-approval list changes, an archived approver is included, and a
/// later boot does not hand back takings an owner has since taken away.
///
/// The 0073, 0074, 0076, 0084, 0086 and 0087 markers are written before the boot, so the boot runs
/// 0079 alone against the roles as they were authored.
#[test]
#[expect(
    clippy::too_many_lines,
    reason = "one boot over five roles and the boot after it, checked against the same rows"
)]
fn existing_approvers_who_close_a_shift_see_takings_once_and_no_other_role_does() {
    const SUPERVISOR: &str = "01ROLE000000000000000000V3";
    const CASHIER: &str = "01ROLE000000000000000000C3";
    const BOOKKEEPER: &str = "01ROLE000000000000000000B3";
    const TRAINEE: &str = "01ROLE000000000000000000T3";
    const RETIRED: &str = "01ROLE000000000000000000R3";
    const TAKINGS: &str = "reports.takings.view";
    block_on(async {
        let (store, admin) = prepared().await.expect("prepare the database");
        admin
            .execute(
                "INSERT INTO data_migrations (name) \
                 VALUES ('0073_roles_keep_every_till_action'), \
                        ('0074_role_permissions_with_approval'), \
                        ('0076_roles_can_waive_a_fee'), \
                        ('0084_roles_can_manage_another_tills_drawer'), \
                        ('0086_roles_can_override_an_assigned_drawer'), \
                        ('0087_roles_can_void_a_paid_bill')",
                &[],
            )
            .await
            .expect("0073, 0074, 0076, 0084, 0086 and 0087 have already run");
        let people = store.people();
        // (role, name, direct, with approval), each the one case it is named for.
        let authored = [
            // Closes a shift and voids a bill directly: an approver who closes shifts. Listed
            // out of order, as a console may have written it.
            (
                SUPERVISOR,
                "Supervisor",
                r#"["cash.shift.open","cash.shift.close","billing.bill.void"]"#,
                r#"["billing.comp.apply"]"#,
            ),
            // Closes a shift directly, and holds every PIN-flagged act only with approval, as a
            // cashier's role does once 0074 has run.
            (
                CASHIER,
                "Cashier",
                r#"["billing.payment.take","cash.shift.close","cash.shift.open"]"#,
                r#"["billing.bill.void","cash.drawer.open_no_sale"]"#,
            ),
            // Approves a refund directly and closes no shift.
            (
                BOOKKEEPER,
                "Bookkeeper",
                r#"["billing.refund.issue"]"#,
                "[]",
            ),
            // Approves a void directly and closes a shift only with approval, which the console
            // refuses to write but a row may still hold.
            (
                TRAINEE,
                "Trainee",
                r#"["billing.bill.void","cash.shift.open"]"#,
                r#"["cash.shift.close"]"#,
            ),
        ];
        for (id, name, direct, approval) in authored {
            people
                .insert_role_template(id, "tenant-a", name, direct, approval, None)
                .await
                .expect("a role");
        }
        // An approver who closes shifts, since retired.
        let retired = people
            .insert_role_template(
                RETIRED,
                "tenant-a",
                "Night manager",
                r#"["cash.drawer.open_no_sale","cash.shift.close"]"#,
                "[]",
                None,
            )
            .await
            .expect("a role since retired");
        people
            .set_role_template(
                "tenant-a",
                RETIRED,
                "Night manager",
                r#"["cash.drawer.open_no_sale","cash.shift.close"]"#,
                "[]",
                "archived",
                None,
                &retired,
            )
            .await
            .expect("archive it");
        let before = people
            .fetch_role_templates("tenant-a")
            .await
            .expect("fetch");
        let as_authored = |id: &str| {
            before
                .iter()
                .find(|role| role.id == id)
                .map(|role| {
                    (
                        role.permissions_json.clone(),
                        role.permissions_with_approval_json.clone(),
                    )
                })
                .expect("the role is listed")
        };

        store
            .migrate()
            .await
            .expect("the boot that runs 0079 first");
        let roles = people
            .fetch_role_templates("tenant-a")
            .await
            .expect("fetch");
        let role = |id: &str| {
            roles
                .iter()
                .find(|role| role.id == id)
                .expect("the role is listed")
        };
        let list = |json: &str| -> Vec<String> {
            serde_json::from_str(json).expect("a permission list is JSON")
        };

        assert_eq!(
            list(&role(SUPERVISOR).permissions_json),
            [
                "billing.bill.void",
                "cash.shift.close",
                "cash.shift.open",
                TAKINGS
            ],
            "an approver who closes shifts sees takings, directly, in byte order"
        );
        assert_eq!(
            list(&role(RETIRED).permissions_json),
            ["cash.drawer.open_no_sale", "cash.shift.close", TAKINGS],
            "an archived approver too, so restoring it restores what it had"
        );
        for (id, why) in [
            (CASHIER, "closes, but approves nothing directly"),
            (BOOKKEEPER, "approves, but closes no shift"),
            (TRAINEE, "closes only with approval"),
        ] {
            assert_eq!(
                role(id).permissions_json,
                as_authored(id).0,
                "{id} {why}, so it is not touched"
            );
        }
        for id in [SUPERVISOR, CASHIER, BOOKKEEPER, TRAINEE, RETIRED] {
            assert_eq!(
                role(id).permissions_with_approval_json,
                as_authored(id).1,
                "{id}: what it grants with approval is byte-identical"
            );
        }

        // The owner takes the takings off supervisors.
        let supervisor = role(SUPERVISOR);
        people
            .set_role_template(
                "tenant-a",
                SUPERVISOR,
                "Supervisor",
                r#"["billing.bill.void","cash.shift.close","cash.shift.open"]"#,
                &supervisor.permissions_with_approval_json,
                "active",
                None,
                &supervisor.version,
            )
            .await
            .expect("the owner edits the role");

        store.migrate().await.expect("the next boot");
        let after = people
            .fetch_role_template("tenant-a", SUPERVISOR)
            .await
            .expect("fetch")
            .expect("present");
        assert!(
            !list(&after.permissions_json).iter().any(|id| id == TAKINGS),
            "a later boot does not hand back takings the owner took away"
        );
    });
}

/// An assignment read names the person it grants, and still lists one whose employee row is gone.
///
/// The console used to turn an assignment's `employee_id` into a name by searching the tenant's
/// whole roster, which is the read that stops working once the roster is paged
/// ([ADR-0098](../../../../../docs/adr/0098-paged-admin-reads.md), B3-4). The resolution is a join
/// on the read that needs it.
///
/// The second half is why that join is `LEFT`. Nothing declares a foreign key from an assignment
/// to an employee, so an assignment can name a row that is not there; an inner join would drop it
/// from the list, hiding a grant that still exists. It lists with no name instead, and the console
/// falls back to showing the id.
#[test]
fn an_assignment_read_names_the_person_and_still_lists_one_whose_employee_is_gone() {
    block_on(async {
        let (store, admin) = prepared().await.expect("prepare the database");
        let people = store.people();

        people
            .insert("01EMP00000000000000000MAI1", "tenant-a", "C77", "Mai")
            .await
            .expect("insert the employee");
        people
            .insert_assignment(
                "01ASSIGN0000000000000NAMED",
                "tenant-a",
                "01EMP00000000000000000MAI1",
                "01STORE000000000000000000S",
                "01ROLE000000000000000000A1",
            )
            .await
            .expect("assign the person who exists");
        // No `employees` row is ever inserted for this id.
        people
            .insert_assignment(
                "01ASSIGN000000000000DANGLE",
                "tenant-a",
                "01EMP0000000000000000GONE1",
                "01STORE000000000000000000S",
                "01ROLE000000000000000000A1",
            )
            .await
            .expect("assign an id with no employee row");

        let rows = people
            .fetch_assignments_for_store("tenant-a", "01STORE000000000000000000S")
            .await
            .expect("by store");
        assert_eq!(rows.len(), 2, "both grants are listed, resolvable or not");

        let named = rows
            .iter()
            .find(|row| row.id == "01ASSIGN0000000000000NAMED")
            .expect("the resolvable assignment");
        assert_eq!(
            named.employee_name.as_deref(),
            Some("Mai"),
            "the join names the person, so no roster read is needed"
        );
        assert_eq!(named.employee_code.as_deref(), Some("C77"));

        let dangling = rows
            .iter()
            .find(|row| row.id == "01ASSIGN000000000000DANGLE")
            .expect("the assignment whose employee row is gone is still listed");
        assert_eq!(
            dangling.employee_name, None,
            "an unresolvable grant reads as unnamed, not as absent"
        );
        assert_eq!(dangling.employee_code, None);

        // The other direction resolves the same way.
        let by_employee = people
            .fetch_assignments_for_employee("tenant-a", "01EMP00000000000000000MAI1")
            .await
            .expect("by employee");
        assert_eq!(by_employee.len(), 1);
        assert_eq!(
            by_employee.first().expect("a row").employee_name.as_deref(),
            Some("Mai")
        );

        drop(admin);
    });
}

/// Assignments bind a person to a store with a role, read both by store and by employee,
/// tenant-scoped; the same person at the same store is refused; and — unlike employees/roles — an
/// assignment IS removable (a DELETE grant), which offboards the person ([ADR-0070]).
#[test]
fn assignments_bind_read_and_remove_with_delete_grant() {
    block_on(async {
        let (store, admin) = prepared().await.expect("prepare the database");
        let people = store.people();

        people
            .insert_assignment(
                "01ASSIGN00000000000000000A",
                "tenant-a",
                "01EMP0000000000000000000A1",
                "01STORE000000000000000000S",
                "01ROLE000000000000000000A1",
            )
            .await
            .expect("assign");
        // The same person at the same store twice is refused by the unique index, as a conflict
        // rather than an outage, and the first assignment stays as it was.
        let refused = people
            .insert_assignment(
                "01ASSIGN00000000000000000B",
                "tenant-a",
                "01EMP0000000000000000000A1",
                "01STORE000000000000000000S",
                "01ROLE000000000000000000B2",
            )
            .await
            .expect_err("a person is assigned to a store at most once");
        assert_eq!(refused.status(), ErrorStatus::AlreadyExists, "{refused}");

        // Readable both ways, tenant-scoped.
        let at_store = people
            .fetch_assignments_for_store("tenant-a", "01STORE000000000000000000S")
            .await
            .expect("by store");
        assert_eq!(at_store.len(), 1);
        let first = at_store.first().expect("a row");
        assert_eq!(
            (first.id.as_str(), first.role_template_id.as_str()),
            ("01ASSIGN00000000000000000A", "01ROLE000000000000000000A1"),
            "the refused duplicate changed nothing"
        );
        assert_eq!(
            people
                .fetch_assignments_for_employee("tenant-a", "01EMP0000000000000000000A1")
                .await
                .expect("by employee")
                .len(),
            1
        );
        assert!(
            people
                .fetch_assignments_for_store("tenant-b", "01STORE000000000000000000S")
                .await
                .expect("other tenant")
                .is_empty(),
            "another tenant sees none of these assignments"
        );

        // Cross-tenant remove matches nothing; a scoped remove offboards.
        assert!(
            !people
                .delete_assignment("tenant-b", "01ASSIGN00000000000000000A")
                .await
                .expect("cross-tenant remove"),
            "delete is tenant-scoped"
        );
        assert!(
            people
                .delete_assignment("tenant-a", "01ASSIGN00000000000000000A")
                .await
                .expect("remove"),
            "the row was removed"
        );
        assert!(
            people
                .fetch_assignments_for_employee("tenant-a", "01EMP0000000000000000000A1")
                .await
                .expect("after remove")
                .is_empty(),
            "the assignment is gone"
        );

        // Assignments ARE removable — a DELETE grant, unlike employees/roles.
        let can_delete: bool = admin
            .query_one(
                "SELECT has_table_privilege('app_tenant', 'employee_store_assignments', 'DELETE')",
                &[],
            )
            .await
            .expect("privilege check")
            .get(0);
        assert!(
            can_delete,
            "an assignment is removed (offboarding), not archived"
        );
    });
}

/// The two reads a removal and a role's archive need to know which stores to tell
/// ([ADR-0158](../../../../../docs/adr/0158-the-till-enforces-each-persons-own-permissions.md)
/// decision 7): one assignment by its id, and every assignment granting a role. Both are
/// tenant-scoped, and both name the person the way every assignment read does.
#[test]
fn an_assignment_reads_by_id_and_by_role_within_its_tenant() {
    block_on(async {
        let (store, admin) = prepared().await.expect("prepare the database");
        let people = store.people();

        people
            .insert("01EMP00000000000000000LAN1", "tenant-a", "C21", "Lan")
            .await
            .expect("insert the employee");
        for (id, store_id, role) in [
            (
                "01ASSIGN0000000000000ROLE1",
                "01STORE000000000000000000S",
                "01ROLE000000000000000000A1",
            ),
            (
                "01ASSIGN0000000000000ROLE2",
                "01STORE000000000000000000T",
                "01ROLE000000000000000000A1",
            ),
            (
                "01ASSIGN0000000000000ROLE3",
                "01STORE000000000000000000U",
                "01ROLE000000000000000000B2",
            ),
        ] {
            people
                .insert_assignment(id, "tenant-a", "01EMP00000000000000000LAN1", store_id, role)
                .await
                .expect("assign");
        }

        let one = people
            .fetch_assignment("tenant-a", "01ASSIGN0000000000000ROLE2")
            .await
            .expect("by id")
            .expect("the assignment exists");
        assert_eq!(
            (one.scope_kind.as_str(), one.store_id.as_deref()),
            ("STORE", Some("01STORE000000000000000000T")),
            "the store a removal must tell"
        );
        assert_eq!(one.employee_name.as_deref(), Some("Lan"));
        assert!(
            people
                .fetch_assignment("tenant-b", "01ASSIGN0000000000000ROLE2")
                .await
                .expect("other tenant")
                .is_none(),
            "another tenant cannot read it by id"
        );
        assert!(
            people
                .fetch_assignment("tenant-a", "01ASSIGN000000000000NOSUCH")
                .await
                .expect("missing")
                .is_none()
        );

        let mut stores: Vec<String> = people
            .fetch_assignments_for_role("tenant-a", "01ROLE000000000000000000A1")
            .await
            .expect("by role")
            .into_iter()
            .filter_map(|row| row.store_id)
            .collect();
        stores.sort();
        assert_eq!(
            stores,
            vec!["01STORE000000000000000000S", "01STORE000000000000000000T"],
            "every store where the role is held, and none where it is not"
        );
        assert!(
            people
                .fetch_assignments_for_role("tenant-b", "01ROLE000000000000000000A1")
                .await
                .expect("other tenant")
                .is_empty(),
            "another tenant sees none of these grants"
        );

        drop(admin);
    });
}

/// The ids of a read's rows, sorted, so a test can compare reads whatever order they return in.
fn ids_of(rows: Vec<store_postgres::AssignmentRow>) -> Vec<String> {
    let mut ids: Vec<String> = rows.into_iter().map(|row| row.id).collect();
    ids.sort();
    ids
}

/// The wider scopes of migration 0077
/// ([ADR-0158](../../../../../docs/adr/0158-the-till-enforces-each-persons-own-permissions.md)
/// decision 3). An assignment to a store group or to every store is a row of
/// `employee_scope_assignments`, and every assignment read lists it beside the one-store rows. A
/// store's read lists every row that reaches it — its own, its groups' through
/// `store_group_members` in the same statement, and its tenant's — so a membership change moves
/// a group's rows from the next read on. The checks hold each scope to exactly its id, the
/// partial unique indexes allow one row per person and group and one per person tenant-wide, a
/// removal finds a row in either table by its id, and re-running the schema, as every boot does,
/// changes none of it.
#[test]
#[expect(
    clippy::too_many_lines,
    reason = "one table's rules, each step reading the rows the one before wrote"
)]
fn a_wider_assignment_reaches_through_its_group_and_its_tenant() {
    block_on(async {
        let (store, admin) = prepared().await.expect("prepare the database");
        let people = store.people();
        let groups = store.store_groups();
        let (north, south, east) = (
            "01STORE00000000000000000N1",
            "01STORE00000000000000000S1",
            "01STORE00000000000000000E1",
        );
        let airport = "01GROUP0000000000000000AP1";
        let (cashier, lead) = ("01ROLE000000000000000000A1", "01ROLE000000000000000000B2");
        let (alice, bao, cam) = (
            "01EMP0000000000000000000A1",
            "01EMP0000000000000000000B2",
            "01EMP0000000000000000000C3",
        );
        let created = groups
            .insert_group("tenant-a", airport, "Airport", "active")
            .await
            .expect("insert the group")
            .expect("a free id");
        let members = [north.to_owned(), south.to_owned()];
        let Ok(RowUpdate::Updated(version)) = groups
            .set_members_at("tenant-a", airport, &members, &created)
            .await
        else {
            panic!("the membership applies at the group's version");
        };

        people
            .insert_assignment(
                "01ASSIGN0000000000000WIDE1",
                "tenant-a",
                alice,
                north,
                cashier,
            )
            .await
            .expect("one store");
        people
            .insert_scope_assignment(
                "01ASSIGN0000000000000WIDE2",
                "tenant-a",
                bao,
                "STORE_GROUP",
                Some(airport),
                cashier,
            )
            .await
            .expect("a group");
        for (id, tenant) in [
            ("01ASSIGN0000000000000WIDE3", "tenant-a"),
            ("01ASSIGN0000000000000WIDE4", "tenant-b"),
        ] {
            people
                .insert_scope_assignment(id, tenant, cam, "TENANT", None, lead)
                .await
                .expect("every store");
        }

        // Each scope holds exactly its id, and one row per person and group or tenant-wide. A
        // second row at a scope the person holds is a conflict; a row the checks refuse is not
        // one, and stays the database's failure.
        for (id, employee, kind, group, status, why) in [
            (
                "01ASSIGN000000000000BAD01",
                alice,
                "STORE_GROUP",
                None,
                ErrorStatus::Unavailable,
                "a group's row names its group",
            ),
            (
                "01ASSIGN000000000000BAD02",
                alice,
                "TENANT",
                Some(airport),
                ErrorStatus::Unavailable,
                "a tenant-wide row names no group",
            ),
            (
                "01ASSIGN000000000000BAD03",
                alice,
                "STORE",
                None,
                ErrorStatus::Unavailable,
                "a one-store row is not this table's",
            ),
            (
                "01ASSIGN000000000000BAD04",
                bao,
                "STORE_GROUP",
                Some(airport),
                ErrorStatus::AlreadyExists,
                "one per person and group",
            ),
            (
                "01ASSIGN000000000000BAD05",
                cam,
                "TENANT",
                None,
                ErrorStatus::AlreadyExists,
                "one per person tenant-wide",
            ),
        ] {
            let refused = people
                .insert_scope_assignment(id, "tenant-a", employee, kind, group, lead)
                .await
                .expect_err(why);
            assert_eq!(refused.status(), status, "{why}: {refused}");
        }
        people
            .insert_scope_assignment(
                "01ASSIGN0000000000000WIDE5",
                "tenant-a",
                alice,
                "TENANT",
                None,
                lead,
            )
            .await
            .expect("a one-store row and a tenant-wide one, for the same person");

        // A store's read reaches through its groups and its tenant, and no further.
        let at = |store_id: &'static str| people.fetch_assignments_for_store("tenant-a", store_id);
        assert_eq!(
            ids_of(at(north).await.expect("by store")),
            vec![
                "01ASSIGN0000000000000WIDE1",
                "01ASSIGN0000000000000WIDE2",
                "01ASSIGN0000000000000WIDE3",
                "01ASSIGN0000000000000WIDE5",
            ]
        );
        assert_eq!(
            ids_of(at(east).await.expect("by store")),
            vec!["01ASSIGN0000000000000WIDE3", "01ASSIGN0000000000000WIDE5"],
            "not in the group, and none of tenant B's"
        );
        let row = at(south)
            .await
            .expect("by store")
            .into_iter()
            .find(|row| row.id == "01ASSIGN0000000000000WIDE2")
            .expect("the group's row reaches a member");
        assert_eq!(
            (row.scope_kind.as_str(), row.store_id, row.store_group_id),
            ("STORE_GROUP", None, Some(airport.to_owned()))
        );

        // A membership change moves the group's rows from the next read on.
        let moved = [south.to_owned(), east.to_owned()];
        assert!(matches!(
            groups
                .set_members_at("tenant-a", airport, &moved, &version)
                .await
                .expect("move the membership"),
            RowUpdate::Updated(_)
        ));
        assert!(
            !ids_of(at(north).await.expect("by store"))
                .contains(&"01ASSIGN0000000000000WIDE2".to_owned()),
            "the store that left"
        );
        assert!(
            ids_of(at(east).await.expect("by store"))
                .contains(&"01ASSIGN0000000000000WIDE2".to_owned()),
            "the store that joined"
        );

        // The wider rows read by what they name, by person and by role, and by id.
        assert_eq!(
            ids_of(
                people
                    .fetch_assignments_for_group("tenant-a", airport)
                    .await
                    .expect("by group")
            ),
            vec!["01ASSIGN0000000000000WIDE2"]
        );
        assert_eq!(
            ids_of(
                people
                    .fetch_tenant_wide_assignments("tenant-a")
                    .await
                    .expect("tenant-wide")
            ),
            vec!["01ASSIGN0000000000000WIDE3", "01ASSIGN0000000000000WIDE5"]
        );
        assert_eq!(
            ids_of(
                people
                    .fetch_assignments_for_employee("tenant-a", alice)
                    .await
                    .expect("by employee")
            ),
            vec!["01ASSIGN0000000000000WIDE1", "01ASSIGN0000000000000WIDE5"]
        );
        assert_eq!(
            ids_of(
                people
                    .fetch_assignments_for_role("tenant-a", lead)
                    .await
                    .expect("by role")
            ),
            vec!["01ASSIGN0000000000000WIDE3", "01ASSIGN0000000000000WIDE5"]
        );
        let wide = people
            .fetch_assignment("tenant-a", "01ASSIGN0000000000000WIDE3")
            .await
            .expect("by id")
            .expect("the row exists");
        assert_eq!(
            (wide.scope_kind.as_str(), wide.store_id, wide.store_group_id),
            ("TENANT", None, None)
        );

        // A removal finds the row in either table, within its tenant.
        assert!(
            !people
                .delete_assignment("tenant-b", "01ASSIGN0000000000000WIDE2")
                .await
                .expect("cross-tenant remove")
        );
        for id in ["01ASSIGN0000000000000WIDE2", "01ASSIGN0000000000000WIDE1"] {
            assert!(
                people
                    .delete_assignment("tenant-a", id)
                    .await
                    .expect("remove")
            );
            assert!(
                !people
                    .delete_assignment("tenant-a", id)
                    .await
                    .expect("again")
            );
        }

        // The query role sees its own tenant's rows only, and may delete them.
        admin
            .batch_execute("SET ROLE app_tenant")
            .await
            .expect("assume the app_tenant role");
        admin
            .execute(
                "SELECT set_config('app.tenant_id', $1, false)",
                &[&"tenant-b"],
            )
            .await
            .expect("scope the session to tenant B");
        let visible: Vec<String> = admin
            .query("SELECT assignment_id FROM employee_scope_assignments", &[])
            .await
            .expect("readable")
            .iter()
            .map(|row| row.get(0))
            .collect();
        let can_delete: bool = admin
            .query_one(
                "SELECT has_table_privilege('app_tenant', 'employee_scope_assignments', 'DELETE')",
                &[],
            )
            .await
            .expect("privilege check")
            .get(0);
        admin
            .batch_execute("RESET ROLE")
            .await
            .expect("reset the role");
        assert_eq!(visible, vec!["01ASSIGN0000000000000WIDE4".to_owned()]);
        assert!(can_delete, "an assignment is removed, not archived");

        // The schema again, as the next boot applies it: nothing written is lost.
        store.migrate().await.expect("migrate again");
        assert_eq!(
            ids_of(at(east).await.expect("by store")),
            vec!["01ASSIGN0000000000000WIDE3", "01ASSIGN0000000000000WIDE5"]
        );
    });
}
