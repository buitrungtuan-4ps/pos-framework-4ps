// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! Config-tree admin authoring (`/admin/stores/{store_id}/config`): publish, the effective read,
//! concurrent and stale writes, version history and the session the routes need.

use axum::http::StatusCode;
use pos_cloud::{Cloud, http};
use pos_fakes::FakeStore;
use tower::ServiceExt;

use crate::{
    FakeConfigTrees, FakeKeys, FakeRollups, admin_cookie, app_full, config_capabilities_app,
    get_with_cookie, json_body, post_config_with_etag, provisioned_admin, put_json,
    put_with_cookie, put_with_etag, store_id, tenant,
};

// --- Config-tree admin authoring (`/admin/stores/{id}/config`, behind the session guard) --------

#[tokio::test]
async fn config_publish_composes_validates_and_reads_back_effective() {
    let router = http::router(app_full(
        Cloud::new(FakeStore::new()),
        FakeRollups::default(),
        FakeKeys::default(),
        provisioned_admin(),
        FakeConfigTrees::default(),
    ));
    let cookie = admin_cookie(&router).await;
    let tenant_ulid = tenant().as_ulid().to_string();
    let store_ulid = store_id().as_ulid().to_string();
    let base = format!("/admin/stores/{store_ulid}/config");

    // Author the tenant layer, then override one key at the store layer.
    let tenant_doc = serde_json::json!({ "currency_code": "VND", "tips_enabled": false });
    let published = router
        .clone()
        .oneshot(put_with_etag(
            &format!("{base}/tenant?tenant_id={tenant_ulid}"),
            &tenant_doc,
            &cookie,
            "*",
        ))
        .await
        .expect("route the publish");
    assert_eq!(published.status(), StatusCode::OK);
    let published = json_body(published).await;
    let first_version = published["config_version_id"]
        .as_str()
        .expect("a successful publish returns the new version id")
        .to_owned();

    let store_doc = serde_json::json!({ "tips_enabled": true });
    let published2 = router
        .clone()
        .oneshot(put_with_etag(
            &format!("{base}/store?tenant_id={tenant_ulid}"),
            &store_doc,
            &cookie,
            &first_version,
        ))
        .await
        .expect("route the second publish");
    assert_eq!(published2.status(), StatusCode::OK);

    // The effective document is the deep merge, most-specific winning.
    let effective = router
        .oneshot(get_with_cookie(
            &format!("{base}?tenant_id={tenant_ulid}"),
            &cookie,
        ))
        .await
        .expect("route the read");
    assert_eq!(effective.status(), StatusCode::OK);
    assert_eq!(
        json_body(effective).await,
        serde_json::json!({ "currency_code": "VND", "tips_enabled": true }),
        "the store layer overrode the tenant layer"
    );
}

/// A node publish survives another publish landing between its read and its write, and neither
/// operator loses anything.
///
/// This is the case the retry exists for, and the one the old code got wrong: both publishes composed
/// a whole tree from the same stale read, so whichever saved second erased the other's node entirely.
/// The layers are a map, so writes to *different* keys have no reason to conflict — and after this
/// both keys are present.
#[tokio::test]
async fn a_node_publish_survives_a_concurrent_publish_of_a_different_node() {
    let config_trees = FakeConfigTrees::default();
    let router = config_capabilities_app(provisioned_admin(), config_trees.clone());
    let cookie = admin_cookie(&router).await;
    let tenant_ulid = tenant().as_ulid().to_string();
    let store_ulid = store_id().as_ulid().to_string();

    // Arm a competing `locale` publish to land after the next read but before its write.
    *config_trees.interpose.lock().expect("lock") =
        Some(("locale".to_owned(), serde_json::json!({ "country": "VN" })));

    let published = router
        .oneshot(put_with_cookie(
            "/admin/config/capabilities",
            &serde_json::json!({
                "tenant_id": tenant_ulid,
                "store_id": store_ulid,
                "flags": { "tables_enabled": false },
            }),
            &cookie,
        ))
        .await
        .expect("route the capabilities publish");
    assert_eq!(
        published.status(),
        StatusCode::OK,
        "a node publish retries past a concurrent one rather than refusing"
    );

    let stored = config_trees
        .rows
        .lock()
        .expect("lock")
        .get(&(tenant(), store_id()))
        .cloned()
        .expect("the tree is there");
    let layer = &stored.record.layers[2];
    assert!(
        layer.get("tables_enabled").is_some(),
        "this publish's own node landed"
    );
    assert!(
        layer.get("locale").is_some(),
        "and the concurrent publish's node was not erased — the bug this slice removes"
    );
}

/// The authored layer publish is the other half: there a second writer really does destroy work an
/// operator typed, so it is refused rather than retried.
#[tokio::test]
async fn an_authored_layer_publish_made_against_a_stale_read_is_refused() {
    let router = http::router(app_full(
        Cloud::new(FakeStore::new()),
        FakeRollups::default(),
        FakeKeys::default(),
        provisioned_admin(),
        FakeConfigTrees::default(),
    ));
    let cookie = admin_cookie(&router).await;
    let tenant_ulid = tenant().as_ulid().to_string();
    let store_ulid = store_id().as_ulid().to_string();
    let base = format!("/admin/stores/{store_ulid}/config");

    let first = router
        .clone()
        .oneshot(put_with_etag(
            &format!("{base}/store?tenant_id={tenant_ulid}"),
            &serde_json::json!({ "tips_enabled": false }),
            &cookie,
            "*",
        ))
        .await
        .expect("route the first publish");
    assert_eq!(first.status(), StatusCode::OK);
    let first_version = json_body(first).await["config_version_id"]
        .as_str()
        .expect("a version id")
        .to_owned();

    // A second writer still asserting the store had nothing published is refused.
    let stale_wildcard = router
        .clone()
        .oneshot(put_with_etag(
            &format!("{base}/store?tenant_id={tenant_ulid}"),
            &serde_json::json!({ "tips_enabled": true }),
            &cookie,
            "*",
        ))
        .await
        .expect("route the stale publish");
    assert_eq!(stale_wildcard.status(), StatusCode::PRECONDITION_FAILED);

    // And naming the version it read lets it through, moving the tree on.
    let fresh = router
        .clone()
        .oneshot(put_with_etag(
            &format!("{base}/store?tenant_id={tenant_ulid}"),
            &serde_json::json!({ "tips_enabled": true }),
            &cookie,
            &first_version,
        ))
        .await
        .expect("route the current publish");
    assert_eq!(fresh.status(), StatusCode::OK);

    // Replaying the version that just stopped being current is the lost update, refused.
    let replayed = router
        .oneshot(put_with_etag(
            &format!("{base}/store?tenant_id={tenant_ulid}"),
            &serde_json::json!({ "tips_enabled": false }),
            &cookie,
            &first_version,
        ))
        .await
        .expect("route the replay");
    assert_eq!(replayed.status(), StatusCode::PRECONDITION_FAILED);
}

#[tokio::test]
#[expect(
    clippy::too_many_lines,
    reason = "one end-to-end scenario: publish two versions, list them, read one back for the diff \
              view, roll back, and assert the append-only restore — splitting it would duplicate the \
              multi-publish setup"
)]
async fn config_versions_list_read_back_and_roll_back_append_only() {
    let router = http::router(app_full(
        Cloud::new(FakeStore::new()),
        FakeRollups::default(),
        FakeKeys::default(),
        provisioned_admin(),
        FakeConfigTrees::default(),
    ));
    let cookie = admin_cookie(&router).await;
    let tenant_ulid = tenant().as_ulid().to_string();
    let store_ulid = store_id().as_ulid().to_string();
    let base = format!("/admin/stores/{store_ulid}/config");

    // Two published versions: v1 sets the currency, v2 overrides it at the store layer.
    let publish = |doc: serde_json::Value, level: &'static str, etag: String| {
        let router = router.clone();
        let cookie = cookie.clone();
        let uri = format!("{base}/{level}?tenant_id={tenant_ulid}");
        async move {
            let response = router
                .oneshot(put_with_etag(&uri, &doc, &cookie, &etag))
                .await
                .expect("route the publish");
            assert_eq!(response.status(), StatusCode::OK);
            json_body(response).await["config_version_id"]
                .as_str()
                .expect("a version id")
                .to_owned()
        }
    };
    let v1 = publish(
        serde_json::json!({ "currency_code": "VND" }),
        "tenant",
        "*".to_owned(),
    )
    .await;
    let v2 = publish(
        serde_json::json!({ "currency_code": "SGD" }),
        "store",
        v1.clone(),
    )
    .await;

    // The history lists both, newest first, with the latest flagged current.
    let versions = router
        .clone()
        .oneshot(get_with_cookie(
            &format!("{base}/versions?tenant_id={tenant_ulid}"),
            &cookie,
        ))
        .await
        .expect("route the versions list");
    assert_eq!(versions.status(), StatusCode::OK);
    let versions = json_body(versions).await;
    let rows = versions.as_array().expect("array");
    assert_eq!(rows.len(), 2, "both published versions are listed");
    assert_eq!(rows[0]["current"], true, "the newest is current");
    assert_eq!(rows[1]["current"], false);
    assert!(
        rows[0]["at_ms"].as_i64().is_some(),
        "each version carries its instant"
    );

    // v1's effective is readable for the diff view.
    let v1_effective = router
        .clone()
        .oneshot(get_with_cookie(
            &format!("{base}/versions/{v1}?tenant_id={tenant_ulid}"),
            &cookie,
        ))
        .await
        .expect("route the version read");
    assert_eq!(v1_effective.status(), StatusCode::OK);
    assert_eq!(
        json_body(v1_effective).await,
        serde_json::json!({ "currency_code": "VND" }),
        "v1's effective is the tenant-only document"
    );

    // Roll back to v1: a new current version is appended, restoring v1's effective.
    let rollback = router
        .clone()
        .oneshot(post_config_with_etag(
            &format!("{base}/rollback?tenant_id={tenant_ulid}"),
            &serde_json::json!({ "version_id": v1 }),
            &cookie,
            &v2,
        ))
        .await
        .expect("route the rollback");
    assert_eq!(rollback.status(), StatusCode::OK);
    let restored = json_body(rollback).await["config_version_id"]
        .as_str()
        .expect("a new version id")
        .to_owned();
    assert_ne!(
        restored, v1,
        "rollback appends a new version, never mutates history"
    );

    // The store's current effective is back to v1's, and the history now holds three versions.
    let effective = router
        .clone()
        .oneshot(get_with_cookie(
            &format!("{base}?tenant_id={tenant_ulid}"),
            &cookie,
        ))
        .await
        .expect("route the effective read");
    assert_eq!(
        json_body(effective).await,
        serde_json::json!({ "currency_code": "VND" }),
        "rollback restored v1's effective config"
    );
    let after = router
        .oneshot(get_with_cookie(
            &format!("{base}/versions?tenant_id={tenant_ulid}"),
            &cookie,
        ))
        .await
        .expect("route the versions list");
    assert_eq!(
        json_body(after).await.as_array().expect("array").len(),
        3,
        "the rollback is a third, appended version"
    );
}

#[tokio::test]
async fn an_incoherent_config_is_rejected_with_violations() {
    let router = http::router(app_full(
        Cloud::new(FakeStore::new()),
        FakeRollups::default(),
        FakeKeys::default(),
        provisioned_admin(),
        FakeConfigTrees::default(),
    ));
    let cookie = admin_cookie(&router).await;
    let tenant_ulid = tenant().as_ulid().to_string();
    let store_ulid = store_id().as_ulid().to_string();

    // pay_first_enabled and tables_enabled are mutually exclusive (pos-core §10).
    let bad = serde_json::json!({ "pay_first_enabled": true, "tables_enabled": true });
    let response = router
        .oneshot(put_with_etag(
            &format!("/admin/stores/{store_ulid}/config/store?tenant_id={tenant_ulid}"),
            &bad,
            &cookie,
            "*",
        ))
        .await
        .expect("route the publish");
    assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);
    let body = json_body(response).await;
    assert_eq!(body["error"]["status"], "UNPROCESSABLE", "got {body}");
    assert!(
        body["error"]["message"]
            .as_str()
            .is_some_and(|message| !message.is_empty()),
        "the rejection names the violated rule(s): {body}"
    );
    assert!(
        body["error"]["details"].is_null(),
        "prose violations carry no field, so no `details` is invented for them: {body}"
    );
}

#[tokio::test]
async fn config_routes_require_a_session() {
    let store_ulid = store_id().as_ulid().to_string();
    let tenant_ulid = tenant().as_ulid().to_string();
    let router = http::router(app_full(
        Cloud::new(FakeStore::new()),
        FakeRollups::default(),
        FakeKeys::default(),
        provisioned_admin(),
        FakeConfigTrees::default(),
    ));

    let publish = router
        .clone()
        .oneshot(put_json(
            &format!("/admin/stores/{store_ulid}/config/tenant?tenant_id={tenant_ulid}"),
            &serde_json::json!({ "a": 1 }),
        ))
        .await
        .expect("route the publish");
    assert_eq!(publish.status(), StatusCode::UNAUTHORIZED);

    // And an unpublished store, once past the guard, reads 404 rather than an empty 200.
    let cookie = admin_cookie(&router).await;
    let read = router
        .oneshot(get_with_cookie(
            &format!("/admin/stores/{store_ulid}/config?tenant_id={tenant_ulid}"),
            &cookie,
        ))
        .await
        .expect("route the read");
    assert_eq!(
        read.status(),
        StatusCode::NOT_FOUND,
        "a store with no published config has no effective document"
    );
}
