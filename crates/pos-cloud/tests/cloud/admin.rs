// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The console's own surface (ADR-0067): signing in, setup and the session guard, self-service
//! sessions, TOTP re-enrolment and recovery codes, and `whoami`.

use axum::body::Body;
use axum::http::{Request, StatusCode};
use pos_cloud::auth::admin::{AdminRole, AdminStore, NewAdminSession, hash_session_token};
use pos_cloud::{Cloud, http};
use pos_fakes::FakeStore;
use pos_proto::time::Timestamp;
use tower::ServiceExt;

use crate::{
    ADMIN_PASSWORD, FakeAdmin, FakeKeys, FakeRegistry, FakeRollups, NOW_MS, admin_cookie,
    admin_totp_code, app_with_admin, cookie_pair, delete_with_cookie, get, get_with_cookie,
    json_body, post_json, post_with_cookie, provisioned_admin, provisioned_admin_as, registry_app,
    role_session_cookie,
};

// --- The interactive super-admin surface (`/admin`) ---------------------------------------------

#[tokio::test]
async fn a_correct_admin_login_sets_a_host_only_cookie_the_guard_then_accepts() {
    let admin = provisioned_admin();
    let router = http::router(app_with_admin(
        Cloud::new(FakeStore::new()),
        FakeRollups::default(),
        FakeKeys::default(),
        admin,
    ));

    // Log in with the correct password and current code.
    let body = serde_json::json!({ "password": ADMIN_PASSWORD, "totp_code": admin_totp_code() });
    let response = router
        .clone()
        .oneshot(post_json("/admin/login", &body))
        .await
        .expect("route the login");
    assert_eq!(response.status(), StatusCode::NO_CONTENT);
    let set_cookie = response
        .headers()
        .get("set-cookie")
        .expect("a session cookie is set")
        .to_str()
        .expect("ascii");
    assert!(
        set_cookie.starts_with("__Host-pos_admin_session="),
        "the host-only session cookie is issued: {set_cookie}"
    );
    assert!(
        !set_cookie.contains("Domain"),
        "a Domain attribute would leak the session across subdomains: {set_cookie}"
    );
    assert!(set_cookie.contains("Secure") && set_cookie.contains("HttpOnly"));

    // The guard accepts the cookie the login issued.
    let session = router
        .oneshot(get_with_cookie("/admin/session", cookie_pair(set_cookie)))
        .await
        .expect("route the session check");
    assert_eq!(
        session.status(),
        StatusCode::NO_CONTENT,
        "the issued session authenticates the guard"
    );
}

#[tokio::test]
async fn the_admin_session_guard_refuses_a_request_without_a_cookie() {
    let response = http::router(app_with_admin(
        Cloud::new(FakeStore::new()),
        FakeRollups::default(),
        FakeKeys::default(),
        provisioned_admin(),
    ))
    .oneshot(get("/admin/session", None))
    .await
    .expect("route the request");
    assert_eq!(
        response.status(),
        StatusCode::UNAUTHORIZED,
        "no session cookie means no admin session"
    );
}

#[tokio::test]
async fn a_wrong_admin_password_is_refused_and_sets_no_cookie() {
    let body =
        serde_json::json!({ "password": "not-the-password", "totp_code": admin_totp_code() });
    let response = http::router(app_with_admin(
        Cloud::new(FakeStore::new()),
        FakeRollups::default(),
        FakeKeys::default(),
        provisioned_admin(),
    ))
    .oneshot(post_json("/admin/login", &body))
    .await
    .expect("route the login");
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    assert!(
        response.headers().get("set-cookie").is_none(),
        "a refused login issues no session cookie"
    );
}

/// An obviously-fake first-boot setup token; never a real credential.
const SETUP_TOKEN: &str = "a-one-time-setup-token-abc123";

/// A router with first-boot enrolment enabled (or not, per `token`) over an *unprovisioned* admin,
/// returning the admin handle so a test can assert whether a credential got written.
fn setup_router(token: Option<&str>) -> (axum::Router, FakeAdmin) {
    let admin = FakeAdmin::default();
    let app = app_with_admin(
        Cloud::new(FakeStore::new()),
        FakeRollups::default(),
        FakeKeys::default(),
        admin.clone(),
    )
    .with_admin_setup_token(token.map(str::to_owned));
    (http::router(app), admin)
}

#[tokio::test]
async fn first_boot_setup_enrols_the_admin_then_refuses_a_second() {
    let (router, admin) = setup_router(Some(SETUP_TOKEN));
    let body = serde_json::json!({ "setup_token": SETUP_TOKEN, "password": ADMIN_PASSWORD });

    let response = router
        .clone()
        .oneshot(post_json("/admin/setup", &body))
        .await
        .expect("route the setup");
    assert_eq!(
        response.status(),
        StatusCode::CREATED,
        "first-boot enrolment succeeds"
    );
    let enrolment = json_body(response).await;
    let uri = enrolment["otpauth_uri"].as_str().expect("an otpauth uri");
    assert!(
        uri.starts_with("otpauth://totp/Pizza4Ps:super-admin?secret="),
        "the enrolment carries a provisioning uri: {uri}"
    );
    assert!(
        uri.contains("algorithm=SHA1"),
        "the uri fixes HMAC-SHA1 — the algorithm every authenticator app computes (ADR-0034): {uri}"
    );
    assert!(
        enrolment["secret_base32"]
            .as_str()
            .is_some_and(|secret| !secret.is_empty()),
        "a base32 secret is returned for manual entry"
    );
    assert!(
        admin.load_credential().await.expect("load").is_some(),
        "a credential is now provisioned"
    );

    // A second enrolment is refused — first-boot is over, even with the right token.
    let again = router
        .oneshot(post_json("/admin/setup", &body))
        .await
        .expect("route the setup");
    assert_eq!(
        again.status(),
        StatusCode::CONFLICT,
        "a second enrolment against an existing admin is refused"
    );
}

#[tokio::test]
async fn setup_is_404_when_no_token_is_configured() {
    let (router, admin) = setup_router(None);
    let body = serde_json::json!({ "setup_token": "anything", "password": ADMIN_PASSWORD });
    let response = router
        .oneshot(post_json("/admin/setup", &body))
        .await
        .expect("route the setup");
    assert_eq!(
        response.status(),
        StatusCode::NOT_FOUND,
        "setup is off when no token is configured"
    );
    assert!(
        admin.load_credential().await.expect("load").is_none(),
        "nothing was provisioned"
    );
}

#[tokio::test]
async fn setup_with_a_wrong_token_is_401_and_provisions_nothing() {
    let (router, admin) = setup_router(Some(SETUP_TOKEN));
    let body = serde_json::json!({ "setup_token": "the-wrong-token", "password": ADMIN_PASSWORD });
    let response = router
        .oneshot(post_json("/admin/setup", &body))
        .await
        .expect("route the setup");
    assert_eq!(
        response.status(),
        StatusCode::UNAUTHORIZED,
        "a wrong setup token is refused"
    );
    assert!(
        admin.load_credential().await.expect("load").is_none(),
        "a refused setup provisions nothing"
    );
}

#[tokio::test]
async fn setup_with_a_short_password_names_the_field_it_is_about() {
    // A single field out of range is a `400`, not the `422` this answered before ADR-0096: there is
    // a field to go and fix, and the refusal has to say which one.
    let (router, admin) = setup_router(Some(SETUP_TOKEN));
    let body = serde_json::json!({ "setup_token": SETUP_TOKEN, "password": "short" });
    let response = router
        .oneshot(post_json("/admin/setup", &body))
        .await
        .expect("route the setup");
    assert_eq!(
        response.status(),
        StatusCode::BAD_REQUEST,
        "too short a password is refused before anything is written"
    );
    let body = json_body(response).await;
    assert_eq!(body["error"]["status"], "INVALID_ARGUMENT", "got {body}");
    assert_eq!(body["error"]["details"][0]["field"], "password");
    assert_eq!(body["error"]["details"][0]["reason"], "OUT_OF_RANGE");
    assert!(
        admin.load_credential().await.expect("load").is_none(),
        "a refused setup provisions nothing"
    );
}

#[tokio::test]
async fn logout_clears_the_cookie_and_revokes_the_session() {
    let router = http::router(app_with_admin(
        Cloud::new(FakeStore::new()),
        FakeRollups::default(),
        FakeKeys::default(),
        provisioned_admin(),
    ));

    // Log in and capture the session cookie.
    let body = serde_json::json!({ "password": ADMIN_PASSWORD, "totp_code": admin_totp_code() });
    let login = router
        .clone()
        .oneshot(post_json("/admin/login", &body))
        .await
        .expect("route the login");
    let cookie = cookie_pair(
        login
            .headers()
            .get("set-cookie")
            .expect("cookie")
            .to_str()
            .expect("ascii"),
    )
    .to_owned();

    // Log out: the response clears the cookie...
    let logout = router
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/admin/logout")
                .header("cookie", &cookie)
                .body(Body::empty())
                .expect("build the request"),
        )
        .await
        .expect("route the logout");
    assert_eq!(logout.status(), StatusCode::NO_CONTENT);
    let cleared = logout
        .headers()
        .get("set-cookie")
        .expect("logout clears the cookie")
        .to_str()
        .expect("ascii");
    assert!(
        cleared.contains("Max-Age=0"),
        "the cookie is expired: {cleared}"
    );

    // ...and the revoked session no longer authenticates the guard.
    let session = router
        .oneshot(get_with_cookie("/admin/session", &cookie))
        .await
        .expect("route the session check");
    assert_eq!(
        session.status(),
        StatusCode::UNAUTHORIZED,
        "a revoked session is no longer accepted"
    );
}

// --- Self-service sessions (G1 slice 4, ADR-0067) -----------------------------------------------

/// Seeds an extra live session bound to `admin_id` (a second signed-in device), for the session-list
/// tests. The idle window equals the cap, so the seeded session neither slides nor idles out during
/// a test.
async fn seed_extra_session(admin: &FakeAdmin, token: &str, admin_id: &str, ip: Option<&str>) {
    let expiry = Timestamp::from_milliseconds_since_epoch(NOW_MS + 3_600_000).expect("valid");
    admin
        .create_session(NewAdminSession {
            token_hash: hash_session_token(token),
            created_at: Timestamp::from_milliseconds_since_epoch(NOW_MS).expect("valid"),
            expires_at: expiry,
            absolute_expires_at: expiry,
            idle_ttl_ms: 3_600_000,
            admin_id: Some(admin_id.to_owned()),
            ip: ip.map(str::to_owned),
            user_agent: None,
        })
        .await
        .expect("seed extra session");
}

#[tokio::test]
async fn listing_sessions_requires_a_session() {
    let router = registry_app(provisioned_admin(), FakeRegistry::default());
    let denied = router
        .oneshot(get("/admin/sessions", None))
        .await
        .expect("route unauthenticated");
    assert_eq!(denied.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn an_admin_lists_and_revokes_their_own_sessions() {
    let admin = provisioned_admin();
    // Even a viewer — least privilege — manages their own sessions: it is self-service, not
    // role-gated.
    let cookie = role_session_cookie(&admin, AdminRole::Viewer, "current-token").await;
    seed_extra_session(&admin, "phone-token", "id-viewer", Some("203.0.113.4")).await;
    let router = registry_app(admin, FakeRegistry::default());

    let listed = json_body(
        router
            .clone()
            .oneshot(get_with_cookie("/admin/sessions", &cookie))
            .await
            .expect("route list"),
    )
    .await;
    let sessions = listed.as_array().expect("array");
    assert_eq!(sessions.len(), 2, "both of the admin's sessions are listed");
    // Exactly one is flagged as the session making this request.
    let current: Vec<&serde_json::Value> =
        sessions.iter().filter(|s| s["current"] == true).collect();
    assert_eq!(current.len(), 1, "exactly one session is the current one");
    let other = sessions
        .iter()
        .find(|s| s["current"] == false)
        .expect("a non-current session");
    let other_id = other["id"].as_str().expect("a handle").to_owned();

    // Revoke the other device by its opaque handle.
    let revoked = router
        .clone()
        .oneshot(delete_with_cookie(
            &format!("/admin/sessions/{other_id}"),
            &cookie,
        ))
        .await
        .expect("route revoke");
    assert_eq!(revoked.status(), StatusCode::NO_CONTENT);

    let after = json_body(
        router
            .clone()
            .oneshot(get_with_cookie("/admin/sessions", &cookie))
            .await
            .expect("route list"),
    )
    .await;
    let remaining = after.as_array().expect("array");
    assert_eq!(remaining.len(), 1, "only the current session remains");
    assert_eq!(remaining[0]["current"], true);
}

#[tokio::test]
async fn revoking_a_bad_handle_or_a_missing_session() {
    let admin = provisioned_admin();
    let cookie = role_session_cookie(&admin, AdminRole::Owner, "owner-token").await;
    let router = registry_app(admin, FakeRegistry::default());

    // A non-hex handle is a 400.
    let bad = router
        .clone()
        .oneshot(delete_with_cookie("/admin/sessions/not-a-handle", &cookie))
        .await
        .expect("route revoke");
    assert_eq!(bad.status(), StatusCode::BAD_REQUEST);

    // A well-formed handle that names no session of theirs is a 404.
    let absent_handle = "0".repeat(64);
    let absent = router
        .clone()
        .oneshot(delete_with_cookie(
            &format!("/admin/sessions/{absent_handle}"),
            &cookie,
        ))
        .await
        .expect("route revoke");
    assert_eq!(absent.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn revoke_others_signs_out_every_session_but_the_current_one() {
    let admin = provisioned_admin();
    let cookie = role_session_cookie(&admin, AdminRole::Owner, "keep-token").await;
    seed_extra_session(&admin, "device-a", "id-owner", None).await;
    seed_extra_session(&admin, "device-b", "id-owner", None).await;
    let router = registry_app(admin, FakeRegistry::default());

    let before = json_body(
        router
            .clone()
            .oneshot(get_with_cookie("/admin/sessions", &cookie))
            .await
            .expect("route list"),
    )
    .await;
    assert_eq!(before.as_array().expect("array").len(), 3);

    let signed_out = router
        .clone()
        .oneshot(post_with_cookie(
            "/admin/sessions/revoke-others",
            &serde_json::json!({}),
            &cookie,
        ))
        .await
        .expect("route revoke others");
    assert_eq!(signed_out.status(), StatusCode::NO_CONTENT);

    let after = json_body(
        router
            .clone()
            .oneshot(get_with_cookie("/admin/sessions", &cookie))
            .await
            .expect("route list"),
    )
    .await;
    let remaining = after.as_array().expect("array");
    assert_eq!(remaining.len(), 1, "only the current session survives");
    assert_eq!(remaining[0]["current"], true);
}

// --- Self-service security: TOTP re-enrol + recovery codes (G1 slice 6, ADR-0067) --------------

/// A router over a provisioned super-admin with a seeded active owner, so a real sign-in binds the
/// session to an `admin_users` id — the shape the recovery-code and re-enrol tests need.
fn security_router() -> axum::Router {
    // One admin, `id-owner`, holding the shared test credential. It used to be two rows — the
    // super-admin credential the login read, plus a placeholder-credentialled `id-owner` row for the
    // session to point at — because those were separate things. Since ADR-0119 sign-in authenticates
    // against the admin's own row, so the admin who signs in and the admin the route reports are one
    // row, and a placeholder hash here would simply refuse the password.
    registry_app(
        provisioned_admin_as("id-owner", "owner@example.test"),
        FakeRegistry::default(),
    )
}

#[tokio::test]
async fn recovery_codes_generate_once_and_sign_in_in_place_of_totp() {
    let router = security_router();
    let cookie = admin_cookie(&router).await;

    // Generate the codes — returned once, with the count.
    let generated = json_body(
        router
            .clone()
            .oneshot(post_with_cookie(
                "/admin/recovery-codes",
                &serde_json::json!({}),
                &cookie,
            ))
            .await
            .expect("route generate"),
    )
    .await;
    let codes = generated["codes"].as_array().expect("codes array");
    assert_eq!(codes.len(), 10);
    assert_eq!(generated["remaining"], 10);
    let code = codes[0].as_str().expect("a code").to_owned();

    // The status endpoint reports the count, never the codes.
    let status = json_body(
        router
            .clone()
            .oneshot(get_with_cookie("/admin/recovery-codes", &cookie))
            .await
            .expect("route status"),
    )
    .await;
    assert_eq!(status["remaining"], 10);

    // A recovery code signs in in place of the TOTP code.
    let signed_in = router
        .clone()
        .oneshot(post_json(
            "/admin/login",
            &serde_json::json!({ "password": ADMIN_PASSWORD, "recovery_code": code }),
        ))
        .await
        .expect("route recovery login");
    assert_eq!(signed_in.status(), StatusCode::NO_CONTENT);

    // Single-use: the same code cannot sign in again.
    let replay = router
        .clone()
        .oneshot(post_json(
            "/admin/login",
            &serde_json::json!({ "password": ADMIN_PASSWORD, "recovery_code": code }),
        ))
        .await
        .expect("route recovery login");
    assert_eq!(replay.status(), StatusCode::UNAUTHORIZED);

    // One code spent leaves nine.
    let status = json_body(
        router
            .clone()
            .oneshot(get_with_cookie("/admin/recovery-codes", &cookie))
            .await
            .expect("route status"),
    )
    .await;
    assert_eq!(status["remaining"], 9);
}

#[tokio::test]
async fn totp_reenrol_needs_the_current_password() {
    let router = security_router();
    let cookie = admin_cookie(&router).await;

    // Signed in but wrong password: a distinct 403, the knowledge factor not re-proved.
    let refused = router
        .clone()
        .oneshot(post_with_cookie(
            "/admin/totp",
            &serde_json::json!({ "password": "wrong-password" }),
            &cookie,
        ))
        .await
        .expect("route reenrol");
    assert_eq!(refused.status(), StatusCode::FORBIDDEN);

    // Correct password: a fresh one-time enrolment (QR + base32 secret).
    let enrolled = router
        .clone()
        .oneshot(post_with_cookie(
            "/admin/totp",
            &serde_json::json!({ "password": ADMIN_PASSWORD }),
            &cookie,
        ))
        .await
        .expect("route reenrol");
    assert_eq!(enrolled.status(), StatusCode::OK);
    let body = json_body(enrolled).await;
    assert!(
        body["otpauth_uri"]
            .as_str()
            .expect("an otpauth uri")
            .contains("otpauth://totp/"),
    );
    assert!(body["secret_base32"].as_str().is_some());
}

#[tokio::test]
async fn the_self_service_security_routes_require_a_session() {
    let router = security_router();
    for request in [
        post_json("/admin/totp", &serde_json::json!({ "password": "x" })),
        post_json("/admin/recovery-codes", &serde_json::json!({})),
        get("/admin/recovery-codes", None),
    ] {
        let denied = router.clone().oneshot(request).await.expect("route");
        assert_eq!(denied.status(), StatusCode::UNAUTHORIZED);
    }
}

// --- Console identity: whoami (G1 slice 7, ADR-0067) --------------------------------------------

#[tokio::test]
async fn whoami_needs_a_session() {
    let router = security_router();
    let denied = router
        .oneshot(get("/admin/whoami", None))
        .await
        .expect("route whoami");
    assert_eq!(denied.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn whoami_reports_the_acting_admins_identity_and_role() {
    let router = security_router();
    let cookie = admin_cookie(&router).await;
    let me = json_body(
        router
            .oneshot(get_with_cookie("/admin/whoami", &cookie))
            .await
            .expect("route whoami"),
    )
    .await;
    // The session bound to the one active owner, so whoami names that owner — role included, and
    // never a credential (no password hash, no TOTP secret in the safe listing shape).
    assert_eq!(me["id"], "id-owner");
    assert_eq!(me["email"], "owner@example.test");
    assert_eq!(me["role"], "owner");
    assert_eq!(me["status"], "active");
    assert!(me.get("password_phc").is_none());
    assert!(me.get("totp_secret").is_none());
}
