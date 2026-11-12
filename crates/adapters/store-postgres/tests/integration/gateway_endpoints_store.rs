// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! A tenant's gateway endpoints (ADR-0198, migration 0098).

use super::{TENANT_A, TENANT_B, block_on, prepared};

use store_postgres::RowUpdate;

const ID: &str = "01J0000000000000000000GW01";

/// A stored record as `pos-cloud` writes one, its secret already a sealed value. The adapter
/// treats `doc` as opaque JSON, so the test crate needs none of the cloud's types.
fn doc(display_name: &str) -> String {
    serde_json::json!({
        "gateway_endpoint_id": ID,
        "display_name": display_name,
        "url": "https://93.184.216.34/balance",
        "method": "GATEWAY_METHOD_GET",
        "deadline_ms": 3000,
        "enabled": true,
        "sealed_headers": { "x-api-key": "00ff" },
    })
    .to_string()
}

#[test]
fn an_endpoint_is_created_once_edited_at_its_version_and_deleted() {
    block_on(async {
        let (store, _admin) = prepared().await.expect("prepare the database");
        let endpoints = store.gateway_endpoints();

        let version = endpoints
            .insert(TENANT_A, ID, &doc("Demo loyalty"))
            .await
            .expect("insert")
            .expect("the id was free");
        assert_eq!(
            endpoints
                .insert(TENANT_A, ID, &doc("Again"))
                .await
                .expect("insert"),
            None,
            "a taken id is refused, not replaced"
        );

        let listed = endpoints.fetch(TENANT_A).await.expect("list");
        assert_eq!(listed.len(), 1);
        assert!(
            listed
                .first()
                .expect("the row")
                .doc_json
                .contains("Demo loyalty")
        );
        assert!(
            endpoints.fetch(TENANT_B).await.expect("list").is_empty(),
            "another tenant reads nothing"
        );

        let updated = endpoints
            .update_at(TENANT_A, ID, &doc("Demo loyalty, renamed"), &version)
            .await
            .expect("update");
        assert!(matches!(updated, RowUpdate::Updated(_)));
        let stale = endpoints
            .update_at(TENANT_A, ID, &doc("Lost update"), &version)
            .await
            .expect("update");
        assert!(
            matches!(stale, RowUpdate::VersionMismatch),
            "an edit made from a stale read is refused"
        );

        endpoints.delete(TENANT_A, ID).await.expect("delete");
        endpoints
            .delete(TENANT_A, ID)
            .await
            .expect("deleting it again is not an error");
        assert!(
            endpoints
                .fetch_one(TENANT_A, ID)
                .await
                .expect("read")
                .is_none()
        );
    });
}

#[test]
fn the_query_role_sees_only_its_own_tenants_endpoints() {
    block_on(async {
        let (store, admin) = prepared().await.expect("prepare the database");
        let endpoints = store.gateway_endpoints();
        endpoints
            .insert(TENANT_A, ID, &doc("Tenant A's"))
            .await
            .expect("insert A")
            .expect("free");
        endpoints
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
            .query("SELECT tenant_id FROM gateway_endpoints", &[])
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
