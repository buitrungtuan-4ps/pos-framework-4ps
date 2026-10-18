// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! API keys (`/admin/api-keys`), and invitations and admin management (`/admin/invites`,
//! `/admin/admins`, ADR-0067).

use axum::http::StatusCode;
use pos_cloud::auth::admin::{AdminRole, AdminStore, NewAdminUser};
use pos_cloud::dashboard::project;
use pos_cloud::{Cloud, http};
use pos_fakes::FakeStore;
use tower::ServiceExt;

use crate::{
    FakeAdmin, FakeKeys, FakeRegistry, FakeRollups, admin_cookie, app_with_admin, dated,
    delete_with_cookie, get, get_with_cookie, json_body, patch_with_cookie, post_json,
    post_with_cookie, provisioned_admin, registry_app, role_session_cookie, store_id, tenant,
};

// --- API-key provisioning (`/admin/api-keys`, behind the session guard) -------------------------

#[tokio::test]
async fn a_provisioned_key_authenticates_v1_then_stops_after_revoke() {
    let cloud = Cloud::new(FakeStore::new());
    cloud
        .ingest(&dated(1, 3, 2026, 3, 15))
        .await
        .expect("ingest");
    let rollups = FakeRollups::default();
    project(cloud.store(), &rollups, tenant(), store_id())
        .await
        .expect("project");
    let router = http::router(app_with_admin(
        cloud,
        rollups,
        FakeKeys::default(),
        provisioned_admin(),
    ));
    let cookie = admin_cookie(&router).await;

    // Provision a read_rollups key for the tenant.
    let body = serde_json::json!({
        "tenant_id": tenant().as_ulid().to_string(),
        "scopes": ["read_rollups"],
    });
    let created = router
        .clone()
        .oneshot(post_with_cookie("/admin/api-keys", &body, &cookie))
        .await
        .expect("route the provisioning");
    assert_eq!(created.status(), StatusCode::CREATED);
    let created = json_body(created).await;
    let token = created["token"]
        .as_str()
        .expect("a one-time token")
        .to_owned();
    let id = created["id"].as_str().expect("the key id").to_owned();
    assert!(
        token.starts_with("pos_"),
        "the token is the real value, shown once"
    );

    // The issued token authenticates a /v1 read for its tenant.
    let ulid = store_id().as_ulid().to_string();
    let read = router
        .clone()
        .oneshot(get(
            &format!("/v1/stores/{ulid}/rollups/daily"),
            Some(&token),
        ))
        .await
        .expect("route the read");
    assert_eq!(
        read.status(),
        StatusCode::OK,
        "the freshly issued key authenticates the public API"
    );

    // It appears in the tenant's listing, without any secret.
    let list = router
        .clone()
        .oneshot(get_with_cookie(
            &format!("/admin/api-keys?tenant_id={}", tenant().as_ulid()),
            &cookie,
        ))
        .await
        .expect("route the list");
    assert_eq!(list.status(), StatusCode::OK);
    let list = json_body(list).await;
    let keys = list.as_array().expect("an array of summaries");
    assert_eq!(keys.len(), 1);
    assert_eq!(keys[0]["id"], id);
    assert_eq!(keys[0]["scopes"][0], "read_rollups");
    assert!(
        keys[0].get("secret").is_none() && keys[0].get("secret_hash").is_none(),
        "a listing never carries the secret or its hash"
    );

    // Revoke it, and the same token no longer authenticates.
    let revoked = router
        .clone()
        .oneshot(delete_with_cookie(
            &format!("/admin/api-keys/{id}"),
            &cookie,
        ))
        .await
        .expect("route the revoke");
    assert_eq!(revoked.status(), StatusCode::NO_CONTENT);
    let read_after = router
        .oneshot(get(
            &format!("/v1/stores/{ulid}/rollups/daily"),
            Some(&token),
        ))
        .await
        .expect("route the read");
    assert_eq!(
        read_after.status(),
        StatusCode::UNAUTHORIZED,
        "a revoked key is refused"
    );
}

#[tokio::test]
async fn provisioning_without_a_session_is_unauthorised() {
    let body = serde_json::json!({
        "tenant_id": tenant().as_ulid().to_string(),
        "scopes": ["read_rollups"],
    });
    let response = http::router(app_with_admin(
        Cloud::new(FakeStore::new()),
        FakeRollups::default(),
        FakeKeys::default(),
        provisioned_admin(),
    ))
    .oneshot(post_json("/admin/api-keys", &body))
    .await
    .expect("route the request");
    assert_eq!(
        response.status(),
        StatusCode::UNAUTHORIZED,
        "provisioning is closed without an admin session"
    );
}

#[tokio::test]
async fn provisioning_with_an_unknown_scope_is_rejected() {
    let router = http::router(app_with_admin(
        Cloud::new(FakeStore::new()),
        FakeRollups::default(),
        FakeKeys::default(),
        provisioned_admin(),
    ));
    let cookie = admin_cookie(&router).await;
    let body = serde_json::json!({
        "tenant_id": tenant().as_ulid().to_string(),
        "scopes": ["not_a_real_scope"],
    });
    let response = router
        .oneshot(post_with_cookie("/admin/api-keys", &body, &cookie))
        .await
        .expect("route the request");
    assert_eq!(
        response.status(),
        StatusCode::BAD_REQUEST,
        "an unknown scope name is a 400, never a silent no-op grant"
    );
    let body = json_body(response).await;
    assert_eq!(body["error"]["status"], "INVALID_ARGUMENT", "got {body}");
    assert_eq!(body["error"]["details"][0]["field"], "scopes");
    assert_eq!(body["error"]["details"][0]["reason"], "INVALID_ENUM_VALUE");
}

// --- Invitations and admin management (G1 slice 3, ADR-0067) ------------------------------------

#[tokio::test]
async fn owner_invites_and_the_invitee_self_enrols() {
    let admin = provisioned_admin();
    let cookie = role_session_cookie(&admin, AdminRole::Owner, "owner-token").await;
    let router = registry_app(admin, FakeRegistry::default());

    // The owner invites an ops admin; the single-use token is returned once.
    let invited = router
        .clone()
        .oneshot(post_with_cookie(
            "/admin/invites",
            &serde_json::json!({ "email": "New.Ops@Example.test", "name": "New Ops", "role": "ops" }),
            &cookie,
        ))
        .await
        .expect("route invite");
    assert_eq!(invited.status(), StatusCode::CREATED);
    let body = json_body(invited).await;
    let token = body["token"].as_str().expect("a token").to_owned();
    assert!(body["invite_id"].as_str().is_some());

    // It lists as pending, with the email normalised to lower-case.
    let pending_invites = json_body(
        router
            .clone()
            .oneshot(get_with_cookie("/admin/invites", &cookie))
            .await
            .expect("route list invites"),
    )
    .await;
    assert_eq!(pending_invites.as_array().expect("array").len(), 1);
    assert_eq!(pending_invites[0]["email"], "new.ops@example.test");
    assert_eq!(pending_invites[0]["role"], "ops");

    // The invitee self-enrols with the token — no session — and gets a one-time TOTP enrolment.
    let accepted = router
        .clone()
        .oneshot(post_json(
            "/admin/invites/accept",
            &serde_json::json!({ "token": token, "password": "a-strong-passphrase" }),
        ))
        .await
        .expect("route accept");
    assert_eq!(accepted.status(), StatusCode::CREATED);
    let enrolment = json_body(accepted).await;
    assert!(enrolment["otpauth_uri"].as_str().is_some());
    assert!(enrolment["secret_base32"].as_str().is_some());

    // The new admin is on the roster, the invite is no longer pending, and the token cannot be reused.
    let admin_roster = json_body(
        router
            .clone()
            .oneshot(get_with_cookie("/admin/admins", &cookie))
            .await
            .expect("route roster"),
    )
    .await;
    let has_new = admin_roster
        .as_array()
        .expect("array")
        .iter()
        .any(|entry| entry["email"] == "new.ops@example.test" && entry["role"] == "ops");
    assert!(has_new, "the accepted admin appears on the roster");

    let remaining = json_body(
        router
            .clone()
            .oneshot(get_with_cookie("/admin/invites", &cookie))
            .await
            .expect("route list invites"),
    )
    .await;
    assert!(remaining.as_array().expect("array").is_empty());

    let replay = router
        .oneshot(post_json(
            "/admin/invites/accept",
            &serde_json::json!({ "token": token, "password": "a-strong-passphrase" }),
        ))
        .await
        .expect("route replay");
    assert_eq!(
        replay.status(),
        StatusCode::UNAUTHORIZED,
        "an accepted invite cannot be replayed"
    );
}

#[tokio::test]
async fn an_admin_cannot_invite_an_owner() {
    let admin = provisioned_admin();
    let cookie = role_session_cookie(&admin, AdminRole::Admin, "admin-token").await;
    let router = registry_app(admin, FakeRegistry::default());
    let denied = router
        .oneshot(post_with_cookie(
            "/admin/invites",
            &serde_json::json!({ "email": "boss@example.test", "name": "Boss", "role": "owner" }),
            &cookie,
        ))
        .await
        .expect("route invite");
    assert_eq!(
        denied.status(),
        StatusCode::FORBIDDEN,
        "an admin may not invite an owner"
    );
}

#[tokio::test]
async fn a_viewer_cannot_invite() {
    let admin = provisioned_admin();
    let cookie = role_session_cookie(&admin, AdminRole::Viewer, "viewer-token").await;
    let router = registry_app(admin, FakeRegistry::default());
    let denied = router
        .oneshot(post_with_cookie(
            "/admin/invites",
            &serde_json::json!({ "email": "x@example.test", "name": "X", "role": "viewer" }),
            &cookie,
        ))
        .await
        .expect("route invite");
    assert_eq!(denied.status(), StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn accept_rejects_a_bad_token_and_a_short_password() {
    let admin = provisioned_admin();
    let router = registry_app(admin, FakeRegistry::default());
    let bad_token = router
        .clone()
        .oneshot(post_json(
            "/admin/invites/accept",
            &serde_json::json!({ "token": "not-a-real-token", "password": "a-strong-passphrase" }),
        ))
        .await
        .expect("route accept");
    assert_eq!(
        bad_token.status(),
        StatusCode::UNAUTHORIZED,
        "an unknown token is a generic 401"
    );
    let short_password = router
        .oneshot(post_json(
            "/admin/invites/accept",
            &serde_json::json!({ "token": "not-a-real-token", "password": "short" }),
        ))
        .await
        .expect("route accept");
    // ADR-0096: one field out of range is a `400` naming it, not a `422`.
    assert_eq!(short_password.status(), StatusCode::BAD_REQUEST);
    let body = json_body(short_password).await;
    assert_eq!(
        body["error"]["details"][0]["field"], "password",
        "got {body}"
    );
    assert_eq!(body["error"]["details"][0]["reason"], "OUT_OF_RANGE");
}

#[tokio::test]
async fn the_last_active_owner_cannot_be_demoted_or_suspended() {
    // The one thing this test needs is *exactly one* active owner, so it starts from an empty store
    // and lets `role_session_cookie` seed that owner. `provisioned_admin()` would add the migrated
    // one alongside, making two — and the invariant this asserts would correctly permit the demotion.
    // Nothing here signs in, so no credential is needed.
    let admin = FakeAdmin::default();
    let cookie = role_session_cookie(&admin, AdminRole::Owner, "owner-token").await;
    let router = registry_app(admin, FakeRegistry::default());
    let demote = router
        .clone()
        .oneshot(patch_with_cookie(
            "/admin/admins/id-owner/role",
            &serde_json::json!({ "role": "admin" }),
            &cookie,
        ))
        .await
        .expect("route demote");
    assert_eq!(
        demote.status(),
        StatusCode::CONFLICT,
        "the last active owner cannot be demoted"
    );
    let suspend = router
        .oneshot(patch_with_cookie(
            "/admin/admins/id-owner/status",
            &serde_json::json!({ "status": "suspended" }),
            &cookie,
        ))
        .await
        .expect("route suspend");
    assert_eq!(
        suspend.status(),
        StatusCode::CONFLICT,
        "the last active owner cannot be suspended"
    );
}

#[tokio::test]
async fn an_owner_can_be_demoted_when_another_owner_remains() {
    let admin = provisioned_admin();
    let cookie = role_session_cookie(&admin, AdminRole::Owner, "owner-token").await;
    // A second active owner, so demoting the first no longer removes the last owner.
    admin
        .create_admin_user(NewAdminUser {
            id: "id-owner-2".to_owned(),
            email: "owner2@example.test".to_owned(),
            name: "O2".to_owned(),
            role: AdminRole::Owner,
            password_phc: "$argon2id$not-a-real-hash".to_owned(),
            totp_secret: b"not-a-real-totp-secret".to_vec(),
        })
        .await
        .expect("seed second owner");
    let router = registry_app(admin, FakeRegistry::default());
    let demote = router
        .oneshot(patch_with_cookie(
            "/admin/admins/id-owner-2/role",
            &serde_json::json!({ "role": "admin" }),
            &cookie,
        ))
        .await
        .expect("route demote");
    assert_eq!(
        demote.status(),
        StatusCode::NO_CONTENT,
        "demotion is allowed while another owner remains"
    );
}
