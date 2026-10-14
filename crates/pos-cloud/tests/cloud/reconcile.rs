// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! Reconciliation (`/internal/reconcile`, `/sync/stores/{store_id}/reconcile`,
//! `/admin/reconcile`): the ids the cloud is missing, and the runs a diff recorded.

use std::collections::HashSet;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use pos_cloud::auth::apikey::Scope;
use pos_cloud::{Cloud, http};
use pos_fakes::FakeStore;
use pos_proto::ids::{EventId, StoreId};
use pos_proto::ulid::Ulid;
use tower::ServiceExt;

use crate::{
    FakeAdmin, FakeConfigTrees, FakeKeys, FakeReconcile, FakeRollups, FakeWebhooks, admin_cookie,
    app_all, clock, event_ulid, internal_secret, issue_store_key, json_body, post_internal,
    post_json, post_json_bearer, provisioned_admin, store_id, tenant,
};

/// The main router (for `/admin/login`) merged with the reconcile sub-router, one shared admin store.
fn reconcile_app(admin: FakeAdmin, store: FakeReconcile) -> axum::Router {
    let app = app_all(
        Cloud::new(FakeStore::new()),
        FakeRollups::default(),
        FakeKeys::default(),
        admin.clone(),
        FakeConfigTrees::default(),
        FakeWebhooks::default(),
    );
    http::router(app).merge(http::reconcile_router(
        store,
        admin,
        clock(),
        Some(internal_secret()),
        FakeKeys::default(),
    ))
}

#[tokio::test]
async fn reconcile_returns_only_the_ids_the_cloud_is_missing() {
    // The cloud holds 1 and 3; the edge reports holding 1, 2, 3, 4 — so 2 and 4 must be re-pushed.
    let present: HashSet<EventId> = [1_u128, 3]
        .into_iter()
        .map(|n| EventId::new(Ulid::from_u128(n)))
        .collect();
    let store = FakeReconcile::with_present(present);
    let router = http::reconcile_router(
        store.clone(),
        provisioned_admin(),
        clock(),
        Some(internal_secret()),
        FakeKeys::default(),
    );
    let body = serde_json::json!({
        "tenant_id": tenant().as_ulid().to_string(),
        "store_id": store_id().as_ulid().to_string(),
        "event_ids": [event_ulid(1), event_ulid(2), event_ulid(3), event_ulid(4)],
    });
    let response = router
        .oneshot(post_internal("/internal/reconcile", &body))
        .await
        .expect("route the reconcile");
    assert_eq!(response.status(), StatusCode::OK);
    let missing = json_body(response).await["missing"]
        .as_array()
        .expect("a missing array")
        .iter()
        .map(|value| value.as_str().expect("a string").to_owned())
        .collect::<Vec<_>>();
    assert_eq!(
        missing,
        vec![event_ulid(2), event_ulid(4)],
        "only the ids the cloud lacks are returned, in the manifest's order"
    );
    // The diff also recorded a run: four ids offered, two missing.
    let recorded = store.recorded();
    assert_eq!(recorded.len(), 1, "one run was recorded for the diff");
    let (recorded_tenant, run) = &recorded[0];
    assert_eq!(*recorded_tenant, tenant());
    assert_eq!(run.store, store_id());
    assert_eq!(run.candidates_offered, 4);
    assert_eq!(run.missing_found, 2);
}

#[tokio::test]
async fn a_store_reconciles_over_sync_with_its_own_key() {
    // Production-readiness R3. ADR-0040 called reconciliation edge-initiated and put it on
    // `/internal/*` — which the shipped proxy denies to every off-box caller, and a store is one by
    // definition. The store's own door takes no shared secret: its tenant is the grant's and its
    // store is the path's, checked against the key's binding.
    let present: HashSet<EventId> = [1_u128, 3]
        .into_iter()
        .map(|n| EventId::new(Ulid::from_u128(n)))
        .collect();
    let store = FakeReconcile::with_present(present);
    let keys = FakeKeys::default();
    let router = http::reconcile_router(
        store.clone(),
        provisioned_admin(),
        clock(),
        Some(internal_secret()),
        keys.clone(),
    );
    let store_ulid = store_id().as_ulid().to_string();
    let uri = format!("/sync/stores/{store_ulid}/reconcile");
    let manifest = serde_json::json!({
        "event_ids": [event_ulid(1), event_ulid(2), event_ulid(3), event_ulid(4)],
    });

    // No bearer at all, and a key scoped to another store, are both closed.
    let anon = router
        .clone()
        .oneshot(post_json(&uri, &manifest))
        .await
        .expect("route");
    assert_eq!(anon.status(), StatusCode::UNAUTHORIZED);
    let elsewhere = issue_store_key(
        &keys,
        tenant(),
        StoreId::new(Ulid::from_u128(0x0000_E15E)),
        &[Scope::ReadConfig],
    );
    let wrong_store = router
        .clone()
        .oneshot(post_json_bearer(&uri, &manifest, &elsewhere))
        .await
        .expect("route");
    assert_eq!(
        wrong_store.status(),
        StatusCode::FORBIDDEN,
        "a key bound to another store cannot reconcile this one's log"
    );

    // This store's own key gets the same diff `/internal/reconcile` gives.
    let token = issue_store_key(&keys, tenant(), store_id(), &[Scope::ReadConfig]);
    let response = router
        .oneshot(post_json_bearer(&uri, &manifest, &token))
        .await
        .expect("route the reconcile");
    assert_eq!(response.status(), StatusCode::OK);
    let missing = json_body(response).await["missing"]
        .as_array()
        .expect("a missing array")
        .iter()
        .map(|value| value.as_str().expect("a string").to_owned())
        .collect::<Vec<_>>();
    assert_eq!(
        missing,
        vec![event_ulid(2), event_ulid(4)],
        "the same diff, through the door a store can actually reach"
    );
    let recorded = store.recorded();
    assert_eq!(
        recorded.len(),
        1,
        "and it records a run like the other door"
    );
    let (recorded_tenant, run) = &recorded[0];
    assert_eq!(
        *recorded_tenant,
        tenant(),
        "the tenant is the grant's, never a body's"
    );
    assert_eq!(run.store, store_id());
    assert_eq!(run.candidates_offered, 4);
    assert_eq!(run.missing_found, 2);
}

#[tokio::test]
async fn reconcile_rejects_a_malformed_id() {
    let store = FakeReconcile::default();
    let router = http::reconcile_router(
        store.clone(),
        provisioned_admin(),
        clock(),
        Some(internal_secret()),
        FakeKeys::default(),
    );
    let body = serde_json::json!({
        "tenant_id": tenant().as_ulid().to_string(),
        "store_id": store_id().as_ulid().to_string(),
        "event_ids": ["not-a-ulid"],
    });
    let response = router
        .oneshot(post_internal("/internal/reconcile", &body))
        .await
        .expect("route the reconcile");
    assert_eq!(
        response.status(),
        StatusCode::BAD_REQUEST,
        "a manifest carrying a non-ULID id is rejected, not silently dropped"
    );
    assert!(
        store.recorded().is_empty(),
        "a rejected manifest records no run"
    );
}

#[tokio::test]
async fn reconcile_history_lists_the_runs_a_diff_recorded() {
    let store = FakeReconcile::with_present(HashSet::new());
    let router = reconcile_app(provisioned_admin(), store);
    let cookie = admin_cookie(&router).await;
    let tenant_ulid = tenant().as_ulid().to_string();
    let store_ulid = store_id().as_ulid().to_string();

    // Run a diff so there is a run to read back: two ids offered, both missing (empty cloud).
    let body = serde_json::json!({
        "tenant_id": tenant_ulid,
        "store_id": store_ulid,
        "event_ids": [event_ulid(7), event_ulid(9)],
    });
    let diff = router
        .clone()
        .oneshot(post_internal("/internal/reconcile", &body))
        .await
        .expect("route the reconcile");
    assert_eq!(diff.status(), StatusCode::OK);

    // The console read lists it.
    let listed = router
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri(format!("/admin/reconcile?tenant_id={tenant_ulid}"))
                .header("cookie", &cookie)
                .body(Body::empty())
                .expect("build the request"),
        )
        .await
        .expect("route the history read");
    assert_eq!(listed.status(), StatusCode::OK);
    let runs = json_body(listed).await;
    let runs = runs.as_array().expect("a runs array");
    assert_eq!(runs.len(), 1, "the one recorded run is listed");
    assert_eq!(runs[0]["store_id"], store_ulid);
    assert_eq!(runs[0]["candidates_offered"], 2);
    assert_eq!(runs[0]["missing_found"], 2);
}

#[tokio::test]
async fn reconcile_history_needs_a_session() {
    let router = reconcile_app(provisioned_admin(), FakeReconcile::default());
    let tenant_ulid = tenant().as_ulid().to_string();
    let response = router
        .oneshot(
            Request::builder()
                .method("GET")
                .uri(format!("/admin/reconcile?tenant_id={tenant_ulid}"))
                .body(Body::empty())
                .expect("build the request"),
        )
        .await
        .expect("route the history read");
    assert_eq!(
        response.status(),
        StatusCode::UNAUTHORIZED,
        "the reconciliation history is behind a session"
    );
}
