// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The scoped per-tenant API-key store (ADR-0037).

use super::{block_on, prepared};

/// Insert reads back by id, lists by tenant without the secret, and revoke is idempotent.
#[test]
fn insert_fetch_list_and_revoke_round_trip() {
    block_on(async {
        let (store, _admin) = prepared().await.expect("prepare the database");
        let keys = store.api_keys();
        let hash: &[u8] = &[3_u8; 32];
        // Two names, one of which this build does not know: the adapter stores whatever it is
        // given and the domain drops unknown names on the way back out (`Scope::from_wire`), so
        // a key issued by a newer cloud round-trips through an older one without loss.
        let scopes = vec!["read_rollups".to_owned(), "read_config".to_owned()];

        keys.insert(
            "KEY0000000000000000000001",
            "TENANT000000000000000000AA",
            Some("STORE0000000000000000000A"),
            hash,
            &scopes,
            None,
        )
        .await
        .expect("insert the key");
        // A second key in the same tenant, bound to no store — the tenant-wide integration key
        // (S1). Both shapes must round-trip, because the guard that reads `store_id` treats
        // `NULL` and a store id as different authorities, not as a present-or-missing detail.
        keys.insert(
            "KEY0000000000000000000002",
            "TENANT000000000000000000AA",
            None,
            hash,
            &scopes,
            None,
        )
        .await
        .expect("insert the tenant-wide key");

        let row = keys
            .fetch("KEY0000000000000000000001")
            .await
            .expect("fetch")
            .expect("the inserted key is present");
        assert_eq!(row.tenant_id, "TENANT000000000000000000AA");
        assert_eq!(
            row.store_id.as_deref(),
            Some("STORE0000000000000000000A"),
            "a store's key round-trips the store it is bound to"
        );
        assert_eq!(row.secret_hash, vec![3_u8; 32]);
        assert!(!row.revoked, "a fresh key is live");
        assert_eq!(row.expires_at_ms, None);

        let tenant_wide = keys
            .fetch("KEY0000000000000000000002")
            .await
            .expect("fetch")
            .expect("the tenant-wide key is present");
        assert_eq!(
            tenant_wide.store_id, None,
            "a tenant-wide key reads back bound to no store"
        );

        let listed = keys
            .list_for_tenant("TENANT000000000000000000AA")
            .await
            .expect("list");
        assert_eq!(listed.len(), 2);
        let only = listed
            .iter()
            .find(|key| key.id == "KEY0000000000000000000001")
            .expect("the store-bound key is listed");
        assert_eq!(only.scopes, scopes, "the granted scopes are listed");
        assert_eq!(
            only.store_id.as_deref(),
            Some("STORE0000000000000000000A"),
            "the listing says which store a key belongs to"
        );

        // Another tenant sees nothing.
        let other = keys
            .list_for_tenant("TENANT000000000000000000BB")
            .await
            .expect("list other");
        assert!(other.is_empty(), "the listing is scoped to the tenant");

        // Revoke: the first call changes a row, the second is a no-op, and the key reads revoked.
        assert!(
            keys.revoke("KEY0000000000000000000001")
                .await
                .expect("revoke")
        );
        assert!(
            !keys
                .revoke("KEY0000000000000000000001")
                .await
                .expect("revoke again"),
            "revoking an already-revoked key changes nothing"
        );
        let row = keys
            .fetch("KEY0000000000000000000001")
            .await
            .expect("fetch")
            .expect("still present");
        assert!(row.revoked, "the key now reads revoked");
    });
}
