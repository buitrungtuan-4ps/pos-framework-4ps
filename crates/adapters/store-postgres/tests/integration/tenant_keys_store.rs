// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! Tenant keys (ADR-0217, migration 0100).

use super::{TENANT_A, TENANT_B, block_on, prepared};

use store_postgres::TenantKeyRow;

fn row(key_version: u32, byte: u8) -> TenantKeyRow {
    TenantKeyRow {
        key_version,
        keeper_version: 1,
        wrapped: vec![byte; 56],
        created_at_ms: 1_800_000_000_000,
    }
}

/// A version is stored once and read back oldest first, each tenant's apart; the tenant-scoped
/// role reads none of them.
#[test]
fn a_tenant_key_version_is_stored_once_and_kept_from_the_tenant_role() {
    block_on(async {
        let (store, admin) = prepared().await.expect("prepare the database");
        let keys = store.tenant_keys();
        assert!(keys.insert(TENANT_A, &row(2, 2)).await.expect("insert"));
        assert!(keys.insert(TENANT_A, &row(1, 1)).await.expect("insert"));
        assert!(
            !keys.insert(TENANT_A, &row(1, 9)).await.expect("insert"),
            "a version another writer stored first is not replaced"
        );
        let held = keys.versions(TENANT_A).await.expect("versions");
        assert_eq!(
            held.iter()
                .map(|key| (key.key_version, key.wrapped.first().copied()))
                .collect::<Vec<_>>(),
            vec![(1, Some(1)), (2, Some(2))]
        );
        assert!(keys.versions(TENANT_B).await.expect("versions").is_empty());

        // A re-wrap replaces one version's bytes; retiring deletes below a version; erasing, all.
        let mut moved = row(1, 7);
        moved.keeper_version = 2;
        assert!(keys.rewrap(TENANT_A, &moved).await.expect("rewrap"));
        assert!(!keys.rewrap(TENANT_B, &moved).await.expect("rewrap"));
        let held = keys.versions(TENANT_A).await.expect("versions");
        assert_eq!(held.first().map(|key| key.keeper_version), Some(2));
        assert_eq!(keys.delete(TENANT_A, Some(2)).await.expect("retire"), 1);
        assert!(keys.insert(TENANT_B, &row(1, 3)).await.expect("insert"));
        assert_eq!(keys.delete(TENANT_A, None).await.expect("erase"), 1);
        assert!(keys.versions(TENANT_A).await.expect("versions").is_empty());
        assert_eq!(
            keys.versions(TENANT_B).await.expect("versions").len(),
            1,
            "another tenant's keys stay"
        );

        admin
            .batch_execute(&format!(
                "SET ROLE app_tenant; SET app.tenant_id = '{TENANT_A}'"
            ))
            .await
            .expect("assume the app_tenant role");
        let read = admin.query("SELECT wrapped FROM tenant_keys", &[]).await;
        admin
            .batch_execute("RESET ROLE; RESET app.tenant_id")
            .await
            .expect("reset the role");
        assert!(
            read.is_err(),
            "the tenant-scoped role is granted nothing on tenant_keys"
        );
    });
}
