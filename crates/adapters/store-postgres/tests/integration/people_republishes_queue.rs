// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The stores whose people the cloud publishes again by itself (migration 0083, ADR-0158).

use super::{block_on, prepared};
use pos_proto::{StoreId, TenantId, Ulid};

/// The migration's own name, which it queues every store under.
const REASON: &str = "0083_people_republishes";

/// A tree's state as the cloud stores it: four layers, no history.
fn tree(tenant_layer: &serde_json::Value, store_layer: &serde_json::Value) -> String {
    serde_json::json!({
        "layers": [tenant_layer, {}, store_layer, {}],
        "history": [],
        "k": 8,
    })
    .to_string()
}

/// Every store whose people have been published, which is a store whose configuration holds a
/// `permissions` roster (a `permissions` object with a `staff` key) on any layer, wherever a
/// rollback left it, is queued once for the cloud to publish its people again, and no other
/// store is. A later boot queues nothing.
///
/// `prepared` truncates `data_migrations` and the queue, so the boot below is the migration's
/// first run, as it is on a database that has never seen the file. The 0084, 0086 and 0087 markers
/// are written first, so what the boot queues is 0083's alone, under its own name.
#[test]
fn the_stores_whose_people_were_published_are_queued_once() {
    let tenant = TenantId::new(Ulid::from_u128(0x0083_7E11));
    let other_tenant = TenantId::new(Ulid::from_u128(0x0083_7E12));
    let store = |n: u128| StoreId::new(Ulid::from_u128(0x0083_0000 + n));
    let empty = serde_json::json!({});
    let people = serde_json::json!({ "permissions": { "staff": [] }, "menu": {} });
    let switch = serde_json::json!({ "permissions": { "enforced": true } });
    let seeded = [
        // People published: queued.
        (tenant, store(1), tree(&empty, &people)),
        // People published beside the switch the settings publish writes: queued.
        (tenant, store(2), tree(&switch, &people)),
        // The switch alone, on the Tenant layer: no people node, not queued.
        (
            tenant,
            store(3),
            tree(&switch, &serde_json::json!({ "menu": {} })),
        ),
        // No `permissions` node anywhere: not queued.
        (tenant, store(4), tree(&empty, &empty)),
        // A null where the node would be is no published roster: not queued.
        (
            tenant,
            store(5),
            tree(&empty, &serde_json::json!({ "permissions": null })),
        ),
        // Rolled back: the restored version whole on the Tenant layer, roster and switch
        // together, and the Store layer empty. Queued.
        (
            tenant,
            store(6),
            tree(
                &serde_json::json!({
                    "permissions": { "enforced": true, "staff": [] },
                    "menu": {},
                }),
                &empty,
            ),
        ),
        // The switch alone typed on the Store layer, with no `staff` key: not queued.
        (tenant, store(7), tree(&empty, &switch)),
        // People published in another tenant: queued.
        (other_tenant, store(1), tree(&empty, &people)),
    ];
    block_on(async {
        let (postgres, admin) = prepared().await.expect("prepare the database");
        admin
            .execute(
                "INSERT INTO data_migrations (name) \
                 VALUES ('0084_roles_can_manage_another_tills_drawer'), \
                        ('0086_roles_can_override_an_assigned_drawer'), \
                        ('0087_roles_can_void_a_paid_bill')",
                &[],
            )
            .await
            .expect("0084, 0086 and 0087 have already run");
        let trees = postgres.config_trees();
        for (tenant_id, store_id, state) in &seeded {
            trees
                .save_state(*tenant_id, *store_id, state, None)
                .await
                .expect("seed a tree");
        }

        postgres
            .migrate()
            .await
            .expect("the boot that runs 0083 first");
        let queue = postgres.people_republishes();
        let queued = queue.pending(None, 100).await.expect("read the queue");
        let keys: Vec<(String, String, &str)> = queued
            .iter()
            .map(|row| {
                (
                    row.tenant_id.clone(),
                    row.store_id.clone(),
                    row.reason.as_str(),
                )
            })
            .collect();
        assert_eq!(
            keys,
            vec![
                (tenant.to_string(), store(1).to_string(), REASON),
                (tenant.to_string(), store(2).to_string(), REASON),
                (tenant.to_string(), store(6).to_string(), REASON),
                (other_tenant.to_string(), store(1).to_string(), REASON),
            ],
            "exactly the stores holding a roster on some layer, in tenant then store order"
        );

        // The drain clears each row with the time it read; the next boot queues none again.
        for row in &queued {
            assert!(
                queue
                    .clear(&row.tenant_id, &row.store_id, row.enqueued_time_ms)
                    .await
                    .expect("clear a row"),
                "a row is cleared with the time read from it, to the millisecond"
            );
        }
        postgres.migrate().await.expect("the next boot");
        assert!(
            queue
                .pending(None, 100)
                .await
                .expect("read the queue")
                .is_empty(),
            "a later boot queues nothing"
        );
    });
}

/// Every marker-gated migration that grants roles a permission, and the queue's: a test of one
/// writes the others' markers before its boot, so the boot runs that one alone.
const MARKED: [&str; 8] = [
    "0073_roles_keep_every_till_action",
    "0074_role_permissions_with_approval",
    "0076_roles_can_waive_a_fee",
    "0079_approvers_who_close_a_shift_see_takings",
    "0083_people_republishes",
    "0084_roles_can_manage_another_tills_drawer",
    "0086_roles_can_override_an_assigned_drawer",
    "0087_roles_can_void_a_paid_bill",
];

/// Every role that exists is given, once, `cash.shift.manage_other_till` on the terms it grants
/// `cash.drawer.open_no_sale` (migration 0084, ADR-0167 decision 3).
#[test]
fn another_tills_drawer_is_granted_once_on_the_drawers_terms_and_its_stores_are_queued() {
    granted_once_on_the_terms_of(
        MARKED[5],
        "cash.shift.manage_other_till",
        "cash.drawer.open_no_sale",
        0x0084,
    );
}

/// Every role that exists is given, once, `cash.drawer.override_assignment` on the same terms
/// (migration 0086, ADR-0167 decision 9).
#[test]
fn an_assigned_drawers_override_is_granted_once_and_its_stores_are_queued() {
    granted_once_on_the_terms_of(
        MARKED[6],
        "cash.drawer.override_assignment",
        "cash.drawer.open_no_sale",
        0x0086,
    );
}

/// Every role that exists is given, once, `billing.bill.void_settled` on the terms it grants
/// `billing.bill.void` (migration 0087, ADR-0184 decision 4).
#[test]
fn a_paid_bills_void_is_granted_once_on_the_voids_terms_and_its_stores_are_queued() {
    granted_once_on_the_terms_of(
        MARKED[7],
        "billing.bill.void_settled",
        "billing.bill.void",
        0x0087,
    );
}

/// The migration `grant` gives every role, once, `permission` on the terms it grants `terms`
/// (`cash.drawer.open_no_sale` or `billing.bill.void`): directly where it grants that directly,
/// and with approval otherwise, an archived role included, and nothing else in either list changes. The same
/// statement queues every store holding a `permissions` roster on some layer under the
/// migration's name, and no other store. A later boot grants nothing again and queues nothing.
///
/// Every other marker in [`MARKED`] is written before the boot, so the boot runs `grant` alone
/// against the roles and the trees as they were. `seed` keeps each case's tenant and stores its
/// own.
#[expect(
    clippy::too_many_lines,
    reason = "one boot over four roles and four stores, and the boot after it"
)]
fn granted_once_on_the_terms_of(grant: &str, permission: &str, terms: &str, seed: u128) {
    const MANAGER: &str = "01ROLE000000000000000000M4";
    const SUPERVISOR: &str = "01ROLE000000000000000000V4";
    const SERVER: &str = "01ROLE000000000000000000S4";
    const RETIRED: &str = "01ROLE000000000000000000R4";
    fn sorted(mut ids: Vec<&str>) -> Vec<&str> {
        ids.sort_unstable();
        ids
    }
    let tenant = TenantId::new(Ulid::from_u128((seed << 16) | 0x7E11));
    let tenant_id = tenant.to_string();
    let store = |n: u128| StoreId::new(Ulid::from_u128((seed << 16) + n));
    let empty = serde_json::json!({});
    let switch = serde_json::json!({ "permissions": { "enforced": true } });
    let seeded = [
        // People published: queued.
        (
            store(1),
            tree(
                &empty,
                &serde_json::json!({ "permissions": { "staff": [] } }),
            ),
        ),
        // Rolled back, the roster on the Tenant layer: queued.
        (
            store(2),
            tree(
                &serde_json::json!({ "permissions": { "enforced": true, "staff": [] } }),
                &empty,
            ),
        ),
        // The switch alone: not queued.
        (store(3), tree(&switch, &empty)),
        // No `permissions` node: not queued.
        (store(4), tree(&empty, &empty)),
    ];
    block_on(async {
        let (postgres, admin) = prepared().await.expect("prepare the database");
        for marker in MARKED.into_iter().filter(|marker| *marker != grant) {
            admin
                .execute("INSERT INTO data_migrations (name) VALUES ($1)", &[&marker])
                .await
                .expect("every other grant and the queue have already run");
        }
        let people = postgres.people();
        // A manager who holds `terms` directly, and so approves it for others.
        let manager_direct =
            serde_json::json!([terms, "cash.shift.close", "cash.shift.open"]).to_string();
        people
            .insert_role_template(
                MANAGER,
                &tenant_id,
                "Manager",
                &manager_direct,
                r#"["billing.comp.apply"]"#,
                Some(500_000),
            )
            .await
            .expect("a manager");
        // A supervisor who holds both `terms` only with somebody's approval.
        people
            .insert_role_template(
                SUPERVISOR,
                &tenant_id,
                "Supervisor",
                r#"["cash.shift.close"]"#,
                r#"["billing.bill.void","cash.drawer.open_no_sale"]"#,
                None,
            )
            .await
            .expect("a supervisor");
        // A server who holds neither, listed out of order as a console may have
        // written it.
        people
            .insert_role_template(
                SERVER,
                &tenant_id,
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
                &tenant_id,
                "Runner",
                r#"["sales.ticket.bump"]"#,
                "[]",
                None,
            )
            .await
            .expect("a role since retired");
        people
            .set_role_template(
                &tenant_id,
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
            .fetch_role_templates(&tenant_id)
            .await
            .expect("fetch");
        let direct_before = |id: &str| -> String {
            before
                .iter()
                .find(|role| role.id == id)
                .map(|role| role.permissions_json.clone())
                .expect("the role is listed")
        };
        let trees = postgres.config_trees();
        for (store_id, state) in &seeded {
            trees
                .save_state(tenant, *store_id, state, None)
                .await
                .expect("seed a tree");
        }

        postgres
            .migrate()
            .await
            .expect("the boot that runs the grant first");
        let roles = people
            .fetch_role_templates(&tenant_id)
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
            sorted(vec![
                terms,
                "cash.shift.close",
                permission,
                "cash.shift.open"
            ]),
            "directly, beside what it is granted on the terms of, in byte order"
        );
        assert_eq!(
            list(&role(MANAGER).permissions_with_approval_json),
            ["billing.comp.apply"],
            "not with approval as well"
        );
        for (id, approved) in [
            (
                SUPERVISOR,
                sorted(vec![
                    "billing.bill.void",
                    "cash.drawer.open_no_sale",
                    permission,
                ]),
            ),
            (SERVER, vec![permission]),
            (RETIRED, vec![permission]),
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

        let queue = postgres.people_republishes();
        let queued = queue.pending(None, 100).await.expect("read the queue");
        let keys: Vec<(String, String, &str)> = queued
            .iter()
            .map(|row| {
                (
                    row.tenant_id.clone(),
                    row.store_id.clone(),
                    row.reason.as_str(),
                )
            })
            .collect();
        assert_eq!(
            keys,
            vec![
                (tenant_id.clone(), store(1).to_string(), grant),
                (tenant_id.clone(), store(2).to_string(), grant),
            ],
            "exactly the stores holding a roster on some layer, under the grant's name"
        );

        // The drain clears the rows, and the owner takes the act off servers altogether.
        for row in &queued {
            assert!(
                queue
                    .clear(&row.tenant_id, &row.store_id, row.enqueued_time_ms)
                    .await
                    .expect("clear a row")
            );
        }
        let server = role(SERVER);
        people
            .set_role_template(
                &tenant_id,
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

        postgres.migrate().await.expect("the next boot");
        let after = people
            .fetch_role_template(&tenant_id, SERVER)
            .await
            .expect("fetch")
            .expect("present");
        assert_eq!(
            list(&after.permissions_with_approval_json),
            Vec::<String>::new(),
            "a later boot does not hand back what the owner took away"
        );
        assert!(
            queue
                .pending(None, 100)
                .await
                .expect("read the queue")
                .is_empty(),
            "and queues nothing"
        );
    });
}
