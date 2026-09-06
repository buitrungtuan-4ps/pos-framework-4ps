// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The table floor routes over HTTP, driven without a socket (P5).
//!
//! Proves the seat → read → clean cycle reaches the application loop and back through the router: a
//! device really can open a table over HTTP, and an illegal move is a `409`, not a `500`.

use std::sync::Arc;

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use pos_core::permission::PermissionSet;
use pos_edge::{
    Edge, EdgeSession, InMemoryQueueNumbers, InMemoryReceipts, Pairing, Sessions, StaffAuth,
    StaffRoster, StoreIdentity, SystemClock,
};
use pos_fakes::FakeStore;
use pos_proto::ClockSource;
use pos_proto::ids::{EmployeeId, StoreId, TableId};
use pos_proto::ulid::Ulid;
use serde_json::json;
use tower::ServiceExt;

/// The badge code + PIN seeded into the store's roster.
const STAFF_CODE: &str = "C01";
const STAFF_PIN: &str = "2468";

/// A real Argon2id PHC hash of `pin`, with a fixed salt so the test needs no RNG.
fn hash_of(pin: &str) -> String {
    use argon2::Argon2;
    use argon2::password_hash::{PasswordHasher, SaltString};
    let salt = SaltString::encode_b64(b"fixed-test-salt!").expect("salt");
    Argon2::default()
        .hash_password(pin.as_bytes(), &salt)
        .expect("hash")
        .to_string()
}

/// The domain router (seeded with one staff member) and a device token that is **paired but not yet
/// signed in** — enough to reach the session routes, not the command routes (S0b, ADR-0084).
async fn paired() -> (Router, String) {
    let identity = StoreIdentity::for_store(StoreId::new(Ulid::from_u128(3)));
    let mut roster = StaffRoster::new();
    roster.insert(
        STAFF_CODE,
        StaffAuth {
            employee_id: Some(EmployeeId::new(Ulid::from_u128(11))),
            permissions: PermissionSet::default(),
            pin_phc: Some(hash_of(STAFF_PIN)),
        },
    );
    let edge = Arc::new(
        Edge::new(
            FakeStore::default(),
            identity,
            EdgeSession::bootstrap().with_staff(roster),
            Arc::new(InMemoryReceipts::new()),
        )
        .expect("seed"),
    );
    let pairing = Arc::new(Pairing::new());
    let now = SystemClock.now();
    let code = pairing.mint(now).expect("mint a pairing code");
    let token = pairing
        .redeem(&code, now)
        .await
        .expect("redeem")
        .token()
        .expect("a fresh code pairs a device")
        .as_str()
        .to_owned();
    (
        pos_edge::http::domain_router(
            edge,
            InMemoryQueueNumbers::new(),
            pairing,
            Arc::new(Sessions::new()),
            &Arc::new(pos_edge::origins::Origins::new()),
        ),
        token,
    )
}

/// Posts a sign-in for `token` with the given code and PIN, returning the status and raw body.
async fn post_sign_in(app: &Router, token: &str, code: &str, pin: &str) -> (StatusCode, String) {
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/session/sign-in")
                .header("authorization", format!("Bearer {token}"))
                .header("content-type", "application/json")
                .body(Body::from(json!({ "code": code, "pin": pin }).to_string()))
                .expect("request builds"),
        )
        .await
        .expect("router responds");
    let status = response.status();
    let bytes = response
        .into_body()
        .collect()
        .await
        .expect("body")
        .to_bytes();
    (
        status,
        String::from_utf8(bytes.to_vec()).unwrap_or_default(),
    )
}

/// A paired device with the seeded staff **signed in** — every command route accepts it.
async fn app() -> (Router, String) {
    let (app, token) = paired().await;
    let (status, _) = post_sign_in(&app, &token, STAFF_CODE, STAFF_PIN).await;
    assert_eq!(status, StatusCode::OK, "the seeded staff signs in");
    (app, token)
}

fn table_path(suffix: &str) -> String {
    let table = TableId::new(Ulid::from_u128(100));
    format!("/api/tables/{table}{suffix}")
}

async fn send(app: Router, token: &str, method: &str, uri: &str) -> (StatusCode, String) {
    let response = app
        .oneshot(
            Request::builder()
                .method(method)
                .uri(uri)
                .header("authorization", format!("Bearer {token}"))
                .body(Body::empty())
                .expect("request builds"),
        )
        .await
        .expect("router responds");
    let status = response.status();
    let bytes = response
        .into_body()
        .collect()
        .await
        .expect("body")
        .to_bytes();
    (
        status,
        String::from_utf8(bytes.to_vec()).unwrap_or_default(),
    )
}

#[tokio::test]
async fn seating_a_table_over_http_opens_it() {
    let (app, token) = app().await;
    let (status, body) = send(app.clone(), &token, "POST", &table_path("/seat")).await;
    assert_eq!(status, StatusCode::OK);
    let view: serde_json::Value = serde_json::from_str(&body).expect("json");
    assert_eq!(view["state"], "TABLE_STATE_OCCUPIED");

    // The same router (sharing the same Edge) now reports the table occupied.
    let (status, body) = send(app, &token, "GET", &table_path("")).await;
    assert_eq!(status, StatusCode::OK);
    let view: serde_json::Value = serde_json::from_str(&body).expect("json");
    assert_eq!(view["state"], "TABLE_STATE_OCCUPIED");
}

#[tokio::test]
async fn an_illegal_move_is_a_conflict_not_a_server_error() {
    // Cleaning a fresh (free) table is not a legal transition.
    let (app, token) = app().await;
    let (status, _) = send(app, &token, "POST", &table_path("/clean")).await;
    assert_eq!(status, StatusCode::CONFLICT);
}

#[tokio::test]
async fn a_non_ulid_table_id_is_a_bad_request() {
    let (app, token) = app().await;
    let (status, _) = send(app, &token, "POST", "/api/tables/not-a-ulid/seat").await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn a_request_without_a_device_token_is_unauthorized() {
    // The gate closes "any host on the store LAN commands the edge" (ADR-0084): a seat with no
    // bearer token is refused before the handler, and a read is refused the same way.
    let (app, _token) = app().await;
    for (method, uri) in [("POST", table_path("/seat")), ("GET", table_path(""))] {
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(method)
                    .uri(uri)
                    .body(Body::empty())
                    .expect("request builds"),
            )
            .await
            .expect("router responds");
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }
}

#[tokio::test]
async fn a_paired_device_with_nobody_signed_in_commands_nothing() {
    // The second gate (S0b, ADR-0084): a paired device whose token is valid but that has no employee
    // signed in is refused a command — and a store read — before the handler, so nothing runs under a
    // forged identity. The device is genuinely paired: the same token signs in successfully below.
    let (app, token) = paired().await;
    for (method, uri) in [("POST", table_path("/seat")), ("GET", table_path(""))] {
        let (status, _) = send(app.clone(), &token, method, &uri).await;
        assert_eq!(
            status,
            StatusCode::FORBIDDEN,
            "a paired device with nobody signed in is forbidden, not unpaired"
        );
    }

    // Sign in, and the very same token now commands.
    let (status, _) = post_sign_in(&app, &token, STAFF_CODE, STAFF_PIN).await;
    assert_eq!(status, StatusCode::OK);
    let (status, _) = send(app, &token, "POST", &table_path("/seat")).await;
    assert_eq!(status, StatusCode::OK, "a signed-in device commands");
}

#[tokio::test]
async fn a_wrong_pin_does_not_sign_in() {
    let (app, token) = paired().await;
    let (status, body) = post_sign_in(&app, &token, STAFF_CODE, "0000").await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    let refused: serde_json::Value = serde_json::from_str(&body).expect("json");
    assert_eq!(refused["outcome"], "wrong");

    // The device stays unsigned (the sign-in failed): a command is still forbidden — the paired gate
    // passes, the signed-in gate does not.
    let (status, _) = send(app, &token, "POST", &table_path("/seat")).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn an_unknown_code_is_refused_without_revealing_it() {
    // An unknown code answers exactly like a wrong PIN — no `remaining`, no distinct status — so a
    // probe cannot tell an existing badge code from a missing one.
    let (app, token) = paired().await;
    let (status, body) = post_sign_in(&app, &token, "NOPE", STAFF_PIN).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    let refused: serde_json::Value = serde_json::from_str(&body).expect("json");
    assert_eq!(refused["outcome"], "wrong");
    assert!(
        refused.get("remaining").is_none(),
        "an unknown code reveals no attempt count"
    );
}
