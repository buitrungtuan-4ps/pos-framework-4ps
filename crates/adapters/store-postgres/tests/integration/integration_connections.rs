// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! A tenant's vendor connections (ADR-0153, migration 0070).

use super::{TENANT_A, TENANT_B, block_on, prepared};

use store_postgres::RowUpdate;

const ID: &str = "01J0000000000000000000CONN";

/// A stored record as `pos-cloud` writes one, its secret already a sealed value. The adapter
/// treats `doc` as opaque JSON, so the test crate needs none of the cloud's types.
fn doc(display_name: &str) -> String {
    serde_json::json!({
        "connection_id": ID,
        "provider_id": "courier.ahamove",
        "scope_level": "CONNECTION_SCOPE_TENANT",
        "display_name": display_name,
        "enabled": true,
        "settings": { "base_url": "https://api.example" },
        "sealed_secrets": { "api_key": "00ff" },
    })
    .to_string()
}

#[test]
fn a_connection_is_created_once_edited_at_its_version_and_deleted() {
    block_on(async {
        let (store, _admin) = prepared().await.expect("prepare the database");
        let connections = store.connections();

        let version = connections
            .insert(TENANT_A, ID, &doc("Ahamove"))
            .await
            .expect("insert")
            .expect("the id was free");
        assert_eq!(
            connections
                .insert(TENANT_A, ID, &doc("Again"))
                .await
                .expect("insert"),
            None,
            "a taken id is refused, not replaced"
        );

        let listed = connections.fetch(TENANT_A).await.expect("list");
        assert_eq!(listed.len(), 1);
        assert!(
            listed
                .first()
                .expect("the row")
                .doc_json
                .contains("Ahamove")
        );
        assert!(
            connections.fetch(TENANT_B).await.expect("list").is_empty(),
            "another tenant reads nothing"
        );

        let updated = connections
            .update_at(TENANT_A, ID, &doc("Ahamove, District 1"), &version)
            .await
            .expect("update");
        assert!(matches!(updated, RowUpdate::Updated(_)));
        let stale = connections
            .update_at(TENANT_A, ID, &doc("Lost update"), &version)
            .await
            .expect("update");
        assert!(
            matches!(stale, RowUpdate::VersionMismatch),
            "an edit made from a stale read is refused"
        );

        connections.delete(TENANT_A, ID).await.expect("delete");
        connections
            .delete(TENANT_A, ID)
            .await
            .expect("deleting it again is not an error");
        assert!(
            connections
                .fetch_one(TENANT_A, ID)
                .await
                .expect("read")
                .is_none()
        );
    });
}

#[test]
fn the_query_role_sees_only_its_own_tenants_connections() {
    block_on(async {
        let (store, admin) = prepared().await.expect("prepare the database");
        let connections = store.connections();
        connections
            .insert(TENANT_A, ID, &doc("Tenant A's"))
            .await
            .expect("insert A")
            .expect("free");
        connections
            .insert(TENANT_B, ID, &doc("Tenant B's"))
            .await
            .expect("insert B")
            .expect("free");

        admin
            .batch_execute("SET ROLE app_tenant")
            .await
            .expect("assume the app_tenant role");
        admin
            .execute(
                "SELECT set_config('app.tenant_id', $1, false)",
                &[&TENANT_A],
            )
            .await
            .expect("scope the session to tenant A");
        let visible: Vec<String> = admin
            .query("SELECT tenant_id FROM integration_connections", &[])
            .await
            .expect("readable")
            .iter()
            .map(|row| row.get(0))
            .collect();
        admin
            .batch_execute("RESET ROLE")
            .await
            .expect("reset the role");
        assert_eq!(visible, vec![TENANT_A.to_owned()]);
    });
}
