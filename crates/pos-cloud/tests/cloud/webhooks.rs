// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The webhook admin routes (`/admin/webhooks`): register, list and delete, the session and the
//! SSRF refusal.

use axum::http::StatusCode;
use pos_cloud::{Cloud, http};
use pos_fakes::FakeStore;
use tower::ServiceExt;

use crate::{
    FakeConfigTrees, FakeKeys, FakeRollups, FakeWebhooks, admin_cookie, app_all, app_with_admin,
    delete_with_cookie, get_with_cookie, json_body, post_json, post_with_cookie, provisioned_admin,
    store_id, tenant,
};

// --- Webhook admin routes (`/admin/webhooks`, behind the session guard) --------------------------

/// Registering returns the signing secret once, the listing shows the endpoint without any secret,
/// and deleting removes it. IP-literal URLs are used throughout so `vet` classifies them without a
/// DNS lookup — the test needs no network.
#[tokio::test]
async fn webhook_register_lists_and_deletes() {
    let router = http::router(app_all(
        Cloud::new(FakeStore::new()),
        FakeRollups::default(),
        FakeKeys::default(),
        provisioned_admin(),
        FakeConfigTrees::default(),
        FakeWebhooks::default(),
    ));
    let cookie = admin_cookie(&router).await;
    let tenant_ulid = tenant().as_ulid().to_string();
    let store_ulid = store_id().as_ulid().to_string();

    // Register a public IP-literal destination (no DNS needed to vet it).
    let body = serde_json::json!({
        "tenant_id": tenant_ulid,
        "store_id": store_ulid,
        "url": "https://93.184.216.34/hook",
    });
    let created = router
        .clone()
        .oneshot(post_with_cookie("/admin/webhooks", &body, &cookie))
        .await
        .expect("route the registration");
    assert_eq!(created.status(), StatusCode::CREATED);
    let created = json_body(created).await;
    let id = created["id"].as_str().expect("an id").to_owned();
    assert!(
        created["signing_secret"]
            .as_str()
            .is_some_and(|secret| secret.len() == 64),
        "the 256-bit signing secret is returned once, as 64 hex chars"
    );
    assert_eq!(created["url"], "https://93.184.216.34/hook");

    // The listing shows the endpoint as metadata only — never a secret.
    let listed = router
        .clone()
        .oneshot(get_with_cookie(
            &format!("/admin/webhooks?tenant_id={tenant_ulid}"),
            &cookie,
        ))
        .await
        .expect("route the listing");
    assert_eq!(listed.status(), StatusCode::OK);
    let listed = json_body(listed).await;
    let rows = listed.as_array().expect("an array");
    assert_eq!(rows.len(), 1, "the one registered endpoint is listed");
    let only = rows.first().expect("one row");
    assert_eq!(only["id"], id);
    assert_eq!(only["url"], "https://93.184.216.34/hook");
    assert!(only["cursor"].is_null(), "nothing delivered yet");
    assert_eq!(only["disabled"], false);
    assert!(
        only.get("secret").is_none() && only.get("signing_secret").is_none(),
        "a listing never carries the signing secret"
    );

    // Delete it; the listing is then empty.
    let deleted = router
        .clone()
        .oneshot(delete_with_cookie(
            &format!("/admin/webhooks/{id}?tenant_id={tenant_ulid}"),
            &cookie,
        ))
        .await
        .expect("route the delete");
    assert_eq!(deleted.status(), StatusCode::NO_CONTENT);
    let listed = router
        .oneshot(get_with_cookie(
            &format!("/admin/webhooks?tenant_id={tenant_ulid}"),
            &cookie,
        ))
        .await
        .expect("route the re-listing");
    assert_eq!(json_body(listed).await.as_array().expect("array").len(), 0);
}

/// Registration is closed without a session, and an inward-pointing or plaintext URL is refused.
#[tokio::test]
async fn webhook_register_requires_a_session_and_refuses_ssrf() {
    let router = http::router(app_with_admin(
        Cloud::new(FakeStore::new()),
        FakeRollups::default(),
        FakeKeys::default(),
        provisioned_admin(),
    ));
    let tenant_ulid = tenant().as_ulid().to_string();
    let store_ulid = store_id().as_ulid().to_string();
    let good = serde_json::json!({
        "tenant_id": tenant_ulid,
        "store_id": store_ulid,
        "url": "https://93.184.216.34/hook",
    });

    // No cookie: closed.
    let unauth = router
        .clone()
        .oneshot(post_json("/admin/webhooks", &good))
        .await
        .expect("route the request");
    assert_eq!(unauth.status(), StatusCode::UNAUTHORIZED);

    let cookie = admin_cookie(&router).await;

    // Loopback: the classic SSRF target, refused as a bad request (IP literal, so no DNS).
    let loopback = serde_json::json!({
        "tenant_id": tenant_ulid,
        "store_id": store_ulid,
        "url": "https://127.0.0.1/hook",
    });
    let refused = router
        .clone()
        .oneshot(post_with_cookie("/admin/webhooks", &loopback, &cookie))
        .await
        .expect("route the request");
    assert_eq!(
        refused.status(),
        StatusCode::BAD_REQUEST,
        "a loopback destination is refused before anything is stored"
    );
    // And the refusal does not repeat the address back. With an IP literal the caller already knows
    // it, but the same body renders a *resolved* address when the host is a name, and this is the
    // assertion that catches the day someone reinstates it.
    let body = String::from_utf8(
        axum::body::to_bytes(refused.into_body(), usize::MAX)
            .await
            .expect("read the refusal body")
            .to_vec(),
    )
    .expect("the refusal body is text");
    assert!(
        !body.contains("127.0.0.1") && !body.contains("loopback"),
        "the refusal must not report what the address was: {body}"
    );
    // And it is the envelope now, naming the field without re-opening the oracle the message
    // closed: a resolution refusal and a not-resolved refusal share one reason.
    let envelope: serde_json::Value =
        serde_json::from_str(&body).expect("the refusal is an envelope");
    assert_eq!(
        envelope["error"]["status"], "INVALID_ARGUMENT",
        "got {envelope}"
    );
    assert_eq!(envelope["error"]["details"][0]["field"], "url");
    assert_eq!(
        envelope["error"]["details"][0]["reason"], "FORBIDDEN_DESTINATION",
        "not INVALID_FORMAT — the URL's shape is fine, its destination is not: {envelope}"
    );

    // Plaintext http is refused even to a public address.
    let plaintext = serde_json::json!({
        "tenant_id": tenant_ulid,
        "store_id": store_ulid,
        "url": "http://93.184.216.34/hook",
    });
    let refused = router
        .oneshot(post_with_cookie("/admin/webhooks", &plaintext, &cookie))
        .await
        .expect("route the request");
    assert_eq!(
        refused.status(),
        StatusCode::BAD_REQUEST,
        "a webhook must use https"
    );
}
