// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! Device onboarding (`/sync/stores/{store_id}/devices`, `/admin/devices/proposals`): propose,
//! approve and reject, the print-agent and connection checks, the audit trail and the scopes.

use std::sync::Arc;

use axum::http::StatusCode;
use pos_cloud::audit::{AuditRecorder, AuditSink, AuditStore};
use pos_cloud::auth::apikey::Scope;
use pos_cloud::{Cloud, http};
use pos_fakes::FakeStore;
use tower::ServiceExt;

use crate::{
    FakeAudit, FakeConfigTrees, FakeDevices, FakeKeys, FakeRollups, FakeWebhooks, admin_cookie,
    app_all, clock, device_app, device_publish_app, get, get_with_cookie, issue_key,
    issue_store_key, json_body, post_config_with_etag, post_json, post_json_bearer,
    post_with_cookie, propose_printer, provisioned_admin, store_id, tenant,
};

#[tokio::test]
async fn device_onboarding_propose_then_approve_then_appears_approved() {
    let keys = FakeKeys::default();
    let devices = FakeDevices::default();
    let router = device_app(provisioned_admin(), keys.clone(), devices);
    let cookie = admin_cookie(&router).await;
    let token = issue_store_key(&keys, tenant(), store_id(), &[Scope::ManageDevices]);
    let store_ulid = store_id().as_ulid().to_string();
    let tenant_ulid = tenant().as_ulid().to_string();
    let devices_uri = format!("/sync/stores/{store_ulid}/devices");

    // The store proposes a discovered printer.
    let proposal = serde_json::json!({ "kind": "printer", "name": "Kitchen 1", "address": "192.168.1.50:9100" });
    let created = router
        .clone()
        .oneshot(post_json_bearer(&devices_uri, &proposal, &token))
        .await
        .expect("route the proposal");
    assert_eq!(created.status(), StatusCode::CREATED);
    let created = json_body(created).await;
    assert_eq!(created["status"], "pending");
    let id = created["id"].as_str().expect("an id").to_owned();

    // It shows in the admin pending queue.
    let pending = router
        .clone()
        .oneshot(get_with_cookie(
            &format!("/admin/devices/proposals?tenant_id={tenant_ulid}"),
            &cookie,
        ))
        .await
        .expect("route the queue");
    assert_eq!(pending.status(), StatusCode::OK);
    let queue = json_body(pending).await;
    assert_eq!(queue.as_array().expect("array").len(), 1);
    assert_eq!(queue[0]["id"], id);
    assert_eq!(queue[0]["kind"], "printer");

    // Before approval the store sees no approved devices.
    let before = router
        .clone()
        .oneshot(get(&devices_uri, Some(&token)))
        .await
        .expect("route the store read");
    assert_eq!(
        json_body(before).await.as_array().expect("array").len(),
        0,
        "nothing is usable until an operator approves it"
    );

    // The admin approves; it then appears in the store's approved list.
    let approve = router
        .clone()
        .oneshot(post_with_cookie(
            &format!(
                "/admin/devices/proposals/{id}/approve?tenant_id={tenant_ulid}&connection=network"
            ),
            &serde_json::json!({}),
            &cookie,
        ))
        .await
        .expect("route the approve");
    assert_eq!(approve.status(), StatusCode::NO_CONTENT);
    let after = router
        .oneshot(get(&devices_uri, Some(&token)))
        .await
        .expect("route the store read");
    let approved = json_body(after).await;
    assert_eq!(approved.as_array().expect("array").len(), 1);
    assert_eq!(approved[0]["address"], "192.168.1.50:9100");
    assert_eq!(
        approved[0]["connection"], "network",
        "the store reads back what approval said about how the device is attached (ADR-0100)"
    );
    assert!(
        approved[0]["station_id"].is_null(),
        "no station named means the counter's receipt printer"
    );
}

/// A device cannot be its own print agent (**ADR-0112**).
///
/// The publish would refuse it anyway — a printer is not a terminal — but the refusal is worth
/// making at the pick, where an operator is looking at the screen that caused it.
#[tokio::test]
async fn a_device_cannot_be_its_own_print_agent() {
    let keys = FakeKeys::default();
    let router = device_publish_app(&keys, FakeConfigTrees::default());
    let cookie = admin_cookie(&router).await;
    let token = issue_store_key(&keys, tenant(), store_id(), &[Scope::ManageDevices]);
    let store_ulid = store_id().as_ulid().to_string();
    let tenant_ulid = tenant().as_ulid().to_string();

    let counter = propose_printer(&router, &token, &store_ulid, "Counter", "/dev/usb/lp0").await;
    let refused = router
        .oneshot(post_config_with_etag(
            &format!("/admin/devices/proposals/{counter}/agent"),
            &serde_json::json!({
                "tenant_id": tenant_ulid.clone(),
                "agent_device_id": counter.clone(),
            }),
            &cookie,
            "1",
        ))
        .await
        .expect("route the self-reference");
    assert_eq!(refused.status(), StatusCode::BAD_REQUEST);
    let body = json_body(refused).await;
    assert_eq!(body["error"]["details"][0]["field"], "agent_device_id");
    assert_eq!(body["error"]["details"][0]["reason"], "SELF_REFERENCE");
}

/// Approving without saying how the device is attached is refused, not guessed at (**ADR-0100**).
///
/// The guess would have to be `network`, which is safe — a network device never opens a cash drawer
/// — and silently wrong for the store whose receipt printer is on USB with a drawer under it. The
/// refusal names the field, so the console can point at it.
#[tokio::test]
async fn approving_a_device_without_a_connection_is_refused() {
    let keys = FakeKeys::default();
    let devices = FakeDevices::default();
    let router = device_app(provisioned_admin(), keys.clone(), devices);
    let cookie = admin_cookie(&router).await;
    let token = issue_store_key(&keys, tenant(), store_id(), &[Scope::ManageDevices]);
    let store_ulid = store_id().as_ulid().to_string();
    let tenant_ulid = tenant().as_ulid().to_string();

    let proposal = serde_json::json!({
        "kind": "printer", "name": "Counter", "address": "192.168.1.51:9100"
    });
    let created = router
        .clone()
        .oneshot(post_json_bearer(
            &format!("/sync/stores/{store_ulid}/devices"),
            &proposal,
            &token,
        ))
        .await
        .expect("route the propose");
    let id = json_body(created).await["id"]
        .as_str()
        .expect("an id")
        .to_owned();

    let refused = router
        .clone()
        .oneshot(post_with_cookie(
            &format!("/admin/devices/proposals/{id}/approve?tenant_id={tenant_ulid}"),
            &serde_json::json!({}),
            &cookie,
        ))
        .await
        .expect("route the approve");
    assert_eq!(refused.status(), StatusCode::BAD_REQUEST);

    let unknown = router
        .oneshot(post_with_cookie(
            &format!(
                "/admin/devices/proposals/{id}/approve?tenant_id={tenant_ulid}&connection=carrier-pigeon"
            ),
            &serde_json::json!({}),
            &cookie,
        ))
        .await
        .expect("route the approve");
    assert_eq!(
        unknown.status(),
        StatusCode::BAD_REQUEST,
        "a connection this build does not know is refused, not stored"
    );
}

#[tokio::test]
async fn approving_a_device_proposal_records_to_the_audit_trail() {
    let keys = FakeKeys::default();
    let devices = FakeDevices::default();
    let audit = FakeAudit::default();
    let admin = provisioned_admin();
    let app = app_all(
        Cloud::new(FakeStore::new()),
        FakeRollups::default(),
        keys.clone(),
        admin.clone(),
        FakeConfigTrees::default(),
        FakeWebhooks::default(),
    );
    let sink: Arc<dyn AuditRecorder> = Arc::new(AuditSink::new(audit.clone()));
    let router = http::router(app).merge(http::device_router(
        devices,
        admin,
        keys.clone(),
        clock(),
        sink,
    ));
    let cookie = admin_cookie(&router).await;
    let token = issue_store_key(&keys, tenant(), store_id(), &[Scope::ManageDevices]);
    let store_ulid = store_id().as_ulid().to_string();
    let tenant_ulid = tenant().as_ulid().to_string();

    let created = router
        .clone()
        .oneshot(post_json_bearer(
            &format!("/sync/stores/{store_ulid}/devices"),
            &serde_json::json!({ "kind": "printer", "name": "Kitchen 1", "address": "192.168.1.50:9100" }),
            &token,
        ))
        .await
        .expect("route the proposal");
    assert_eq!(created.status(), StatusCode::CREATED);
    let id = json_body(created).await["id"]
        .as_str()
        .expect("an id")
        .to_owned();

    let approve = router
        .clone()
        .oneshot(post_with_cookie(
            &format!(
                "/admin/devices/proposals/{id}/approve?tenant_id={tenant_ulid}&connection=network"
            ),
            &serde_json::json!({}),
            &cookie,
        ))
        .await
        .expect("route the approve");
    assert_eq!(approve.status(), StatusCode::NO_CONTENT);

    let recorded = audit.list(None, 10).await.expect("list audit entries");
    let entry = recorded
        .iter()
        .find(|entry| entry.entity_type == "device_proposal")
        .expect("the resolve was recorded");
    assert_eq!(
        entry.action, "device_proposal.approve",
        "approve is distinguished from reject"
    );
    assert_eq!(
        entry.entity_id, id,
        "the resolved proposal's id is recorded"
    );
    assert_eq!(
        entry.tenant_id.map(|id| id.to_string()),
        Some(tenant_ulid),
        "the entry is scoped to the tenant"
    );
    assert!(
        !entry.actor.email.is_empty(),
        "the resolving admin is snapshotted onto the entry"
    );
}

#[tokio::test]
async fn device_routes_enforce_their_scopes_and_the_session() {
    let keys = FakeKeys::default();
    let router = device_app(provisioned_admin(), keys.clone(), FakeDevices::default());
    let store_ulid = store_id().as_ulid().to_string();
    let tenant_ulid = tenant().as_ulid().to_string();
    let devices_uri = format!("/sync/stores/{store_ulid}/devices");
    let proposal = serde_json::json!({ "kind": "kds", "name": "Expo", "address": "192.168.1.9" });

    // No bearer: closed.
    let anon = router
        .clone()
        .oneshot(post_json(&devices_uri, &proposal))
        .await
        .expect("route");
    assert_eq!(anon.status(), StatusCode::UNAUTHORIZED);

    // A key without manage_devices: forbidden.
    let rollups_only = issue_key(&keys, tenant(), &[Scope::ReadRollups]);
    let wrong = router
        .clone()
        .oneshot(post_json_bearer(&devices_uri, &proposal, &rollups_only))
        .await
        .expect("route");
    assert_eq!(wrong.status(), StatusCode::FORBIDDEN);

    // The admin queue is closed without a session.
    let no_session = router
        .oneshot(get(
            &format!("/admin/devices/proposals?tenant_id={tenant_ulid}"),
            None,
        ))
        .await
        .expect("route");
    assert_eq!(no_session.status(), StatusCode::UNAUTHORIZED);
}
