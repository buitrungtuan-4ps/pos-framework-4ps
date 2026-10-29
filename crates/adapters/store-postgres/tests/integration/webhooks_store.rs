// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The webhook-endpoint store (ADR-0032): registration facts, the admin CRUD's
//! tenant-scoped listing, and the delivery task's fleet-wide enabled load.

use super::{block_on, prepared};
use store_postgres::PostgresWebhooks;

// Obviously-fake, ULID-shaped identifiers for the fixture endpoints — never real key material.
const TENANT_A: &str = "TENANT000000000000000000AA";
const TENANT_B: &str = "TENANT000000000000000000BB";
const STORE_1: &str = "STORE0000000000000000000A1";
const STORE_2: &str = "STORE0000000000000000000B1";
const HOOK_A1: &str = "WEBHOOK00000000000000000A1";
const HOOK_A2: &str = "WEBHOOK00000000000000000A2";
const HOOK_B1: &str = "WEBHOOK00000000000000000B1";

/// Registers two endpoints for tenant A and one for tenant B — the fixture both cases start from.
async fn seed(hooks: &PostgresWebhooks) {
    hooks
        .create(
            HOOK_A1,
            TENANT_A,
            STORE_1,
            "https://a.example/hook1",
            "secret-a1",
        )
        .await
        .expect("register A's first endpoint");
    hooks
        .create(
            HOOK_A2,
            TENANT_A,
            STORE_1,
            "https://a.example/hook2",
            "secret-a2",
        )
        .await
        .expect("register A's second endpoint");
    hooks
        .create(
            HOOK_B1,
            TENANT_B,
            STORE_2,
            "https://b.example/hook",
            "secret-b1",
        )
        .await
        .expect("register B's endpoint");
}

/// The tenant-scoped listing hides the secret and isolates by tenant; the fleet-wide enabled
/// load carries the secret; and a successful delivery advances the cursor.
#[test]
fn list_is_tenant_scoped_and_enabled_load_carries_the_secret() {
    block_on(async {
        let (store, _admin) = prepared().await.expect("prepare the database");
        let hooks = store.webhooks();
        seed(&hooks).await;

        // A sees its two, B sees its one, and the summary row has no secret field at all.
        let listed_a = hooks.list(TENANT_A).await.expect("list A");
        assert_eq!(listed_a.len(), 2, "tenant A sees its two endpoints");
        let mut a_ids: Vec<&str> = listed_a.iter().map(|row| row.id.as_str()).collect();
        a_ids.sort_unstable();
        assert_eq!(a_ids, vec![HOOK_A1, HOOK_A2]);
        assert!(
            listed_a
                .iter()
                .all(|row| row.cursor.is_none() && !row.disabled),
            "freshly-registered endpoints have no cursor and are enabled"
        );

        let listed_b = hooks.list(TENANT_B).await.expect("list B");
        assert_eq!(listed_b.len(), 1, "tenant B sees only its own endpoint");
        assert_eq!(
            listed_b.first().expect("one row").store_id,
            STORE_2,
            "and it is the store B registered against"
        );

        // The fleet-wide enabled load is what the delivery task uses: all three, each in full,
        // including the signing secret it must sign with. It is NOT tenant-scoped.
        let enabled = hooks.fetch_enabled().await.expect("fetch enabled");
        assert_eq!(enabled.len(), 3, "all three endpoints are enabled at first");
        let a1 = enabled
            .iter()
            .find(|row| row.id == HOOK_A1)
            .expect("A's first endpoint is in the enabled load");
        assert_eq!(a1.tenant_id, TENANT_A);
        assert_eq!(a1.secret, "secret-a1", "the secret is loaded for signing");
        assert!(a1.cursor.is_none(), "no cursor until the first delivery");

        // A successful delivery advances the cursor; the next enabled load reflects it.
        hooks
            .advance_cursor(HOOK_A1, "EVENT0000000000000000000X1")
            .await
            .expect("advance A1's cursor");
        let enabled = hooks.fetch_enabled().await.expect("re-fetch enabled");
        let a1 = enabled
            .iter()
            .find(|row| row.id == HOOK_A1)
            .expect("still enabled");
        assert_eq!(
            a1.cursor.as_deref(),
            Some("EVENT0000000000000000000X1"),
            "the cursor advanced to the last delivered event id"
        );
    });
}

/// Disabling drops an endpoint from the enabled load but keeps it listed as disabled; delete is
/// tenant-scoped and a second delete of the same id removes nothing.
#[test]
fn disable_suppresses_and_delete_is_tenant_scoped() {
    block_on(async {
        let (store, _admin) = prepared().await.expect("prepare the database");
        let hooks = store.webhooks();
        seed(&hooks).await;

        // Disabling an endpoint (the breaker's 24-hour auto-disable) drops it from the enabled
        // load but keeps it in the tenant's listing, marked disabled.
        hooks
            .mark_disabled(HOOK_A2, true)
            .await
            .expect("disable A2");
        let enabled = hooks.fetch_enabled().await.expect("enabled after disable");
        assert_eq!(
            enabled.len(),
            2,
            "the disabled endpoint is not delivered to"
        );
        assert!(
            enabled.iter().all(|row| row.id != HOOK_A2),
            "A2 specifically is gone from the enabled load"
        );
        let listed_a = hooks.list(TENANT_A).await.expect("re-list A");
        let a2 = listed_a
            .iter()
            .find(|row| row.id == HOOK_A2)
            .expect("A2 still appears in the listing");
        assert!(a2.disabled, "and it reads disabled");

        // Delete is tenant-scoped: B cannot delete A's endpoint, A can, and a second delete of
        // the same id removes nothing.
        assert!(
            !hooks
                .remove(TENANT_B, HOOK_A1)
                .await
                .expect("B's delete of A's id"),
            "a tenant cannot delete another tenant's endpoint"
        );
        assert!(
            hooks
                .remove(TENANT_A, HOOK_A1)
                .await
                .expect("A deletes its own endpoint"),
            "the owning tenant removes its endpoint"
        );
        assert!(
            !hooks
                .remove(TENANT_A, HOOK_A1)
                .await
                .expect("second delete"),
            "deleting an already-removed endpoint removes nothing"
        );
        assert_eq!(
            hooks
                .list(TENANT_A)
                .await
                .expect("list A after delete")
                .len(),
            1,
            "one of A's two endpoints remains"
        );
    });
}
