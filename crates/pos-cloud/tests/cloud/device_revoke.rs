// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! Remote device retirement (`POST /admin/stores/{store_id}/devices/revoke`, ADR-0118 §6).

use axum::body::Body;
use axum::http::{Request, StatusCode};
use pos_proto::ulid::Ulid;
use tower::ServiceExt;

use crate::{
    FakeConfigTrees, admin_cookie, device_revoke_app, json_body, post_with_cookie,
    provisioned_admin, published_deny_list, store_id, tenant,
};

#[tokio::test]
async fn revoking_a_device_appends_to_the_deny_list_and_never_replaces_it() {
    // The property the whole rail rests on. Two tills retired one after the other must both stay on
    // the list: a publish that replaced the node would drop the first, and a device somebody was
    // told had been retired would be trading again after the store's next pull.
    let admin = provisioned_admin();
    let config = FakeConfigTrees::default();
    let router = device_revoke_app(admin, config.clone());
    let cookie = admin_cookie(&router).await;
    let tenant_ulid = tenant().as_ulid().to_string();
    let store_ulid = store_id().as_ulid().to_string();
    let uri = format!("/admin/stores/{store_ulid}/devices/revoke");
    let first = Ulid::from_u128(0x21).to_string();
    let second = Ulid::from_u128(0x22).to_string();

    let revoke = |device: String| {
        router.clone().oneshot(post_with_cookie(
            &uri,
            &serde_json::json!({ "tenant_id": tenant_ulid, "local_device_id": device }),
            &cookie,
        ))
    };

    let response = revoke(first.clone()).await.expect("route the revoke");
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        published_deny_list(&config, tenant(), store_id()).await,
        vec![first.clone()]
    );

    let response = revoke(second.clone()).await.expect("route the revoke");
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        published_deny_list(&config, tenant(), store_id()).await,
        vec![first.clone(), second.clone()],
        "the second retirement appends; it does not replace the first"
    );

    // Idempotent: re-revoking an id already listed succeeds and adds nothing. A duplicate entry
    // would make the node malformed, and the store refuses a malformed node whole — so a double
    // tap in the console would disarm the very list it was adding to.
    let response = revoke(second.clone()).await.expect("route the revoke");
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        published_deny_list(&config, tenant(), store_id()).await,
        vec![first, second]
    );
}

#[tokio::test]
async fn revoking_a_device_needs_a_session_and_the_store_manage_role() {
    let admin = provisioned_admin();
    let config = FakeConfigTrees::default();
    let router = device_revoke_app(admin, config.clone());
    let tenant_ulid = tenant().as_ulid().to_string();
    let store_ulid = store_id().as_ulid().to_string();
    let uri = format!("/admin/stores/{store_ulid}/devices/revoke");
    let body = serde_json::json!({
        "tenant_id": tenant_ulid,
        "local_device_id": Ulid::from_u128(0x21).to_string()
    });

    let unauth = router
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(&uri)
                .header("content-type", "application/json")
                .body(Body::from(serde_json::to_vec(&body).expect("serialise")))
                .expect("build the request"),
        )
        .await
        .expect("route the revoke");
    assert_eq!(
        unauth.status(),
        StatusCode::UNAUTHORIZED,
        "taking a terminal out of service is closed to an unauthenticated caller"
    );
    assert!(
        published_deny_list(&config, tenant(), store_id())
            .await
            .is_empty(),
        "and nothing was published"
    );
}

#[tokio::test]
async fn a_local_device_id_that_is_not_a_ulid_is_refused_by_name() {
    let admin = provisioned_admin();
    let config = FakeConfigTrees::default();
    let router = device_revoke_app(admin, config.clone());
    let cookie = admin_cookie(&router).await;
    let tenant_ulid = tenant().as_ulid().to_string();
    let store_ulid = store_id().as_ulid().to_string();

    let response = router
        .oneshot(post_with_cookie(
            &format!("/admin/stores/{store_ulid}/devices/revoke"),
            &serde_json::json!({
                "tenant_id": tenant_ulid,
                "local_device_id": "till-by-the-window"
            }),
            &cookie,
        ))
        .await
        .expect("route the revoke");
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let body = json_body(response).await;
    assert!(
        body.to_string().contains("local_device_id"),
        "the refusal names the field: {body}"
    );
    assert!(
        published_deny_list(&config, tenant(), store_id())
            .await
            .is_empty()
    );
}
