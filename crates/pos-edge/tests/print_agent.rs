// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! Binding a terminal's print agent over HTTP ([ADR-0112](../../../docs/adr/0112-print-agents.md)).
//!
//! The claim carries **two** gates, and the tests below are about what each one is for. The paired
//! gate says a box the store admitted is asking. The signed-in-manager gate says a person with
//! standing decided — because binding a terminal is a managerial act performed in front of the
//! machine, and a waiter's tap must not be able to move where the kitchen's tickets print.
//!
//! What neither gate proves is which physical machine is on the other end: the framework has no
//! device attestation, so a manager who signs in on a phone and claims a terminal gets a phone as
//! the agent. These tests pin what the gates *do* buy — that it cannot happen casually, and that a
//! second box cannot quietly take a live binding over.

use std::sync::Arc;

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use pos_core::permission::{Permission, PermissionSet};
use pos_edge::{
    Edge, EdgeSession, InMemoryQueueNumbers, InMemoryReceipts, Pairing, Sessions, StaffAuth,
    StaffRoster, StoreIdentity, SystemClock,
};
use pos_fakes::FakeStore;
use pos_proto::ClockSource;
use pos_proto::ids::{EmployeeId, StoreId};
use pos_proto::ulid::Ulid;
use serde_json::json;
use tower::ServiceExt;

/// A manager (may manage devices) and a waiter (may not).
const MANAGER_CODE: &str = "M01";
const WAITER_CODE: &str = "W01";
const PIN: &str = "2468";

/// The terminal being claimed — a `TERMINAL` entry the console created, as it reaches the store in
/// the published `devices` node.
const TILL: &str = "0000000000000000000000000E";

fn hash_of(pin: &str) -> String {
    use argon2::Argon2;
    use argon2::password_hash::{PasswordHasher, SaltString};
    let salt = SaltString::encode_b64(b"fixed-test-salt!").expect("salt");
    Argon2::default()
        .hash_password(pin.as_bytes(), &salt)
        .expect("hash")
        .to_string()
}

/// The domain router and **two** paired device tokens: the binding is exclusive, and one token
/// cannot prove that.
async fn paired_pair() -> (Router, String, String) {
    let identity = StoreIdentity::for_store(StoreId::new(Ulid::from_u128(3)));
    let mut staff = StaffRoster::new();
    staff.insert(
        MANAGER_CODE,
        StaffAuth {
            employee_id: Some(EmployeeId::new(Ulid::from_u128(11))),
            permissions: [Permission::ManageDevices].into_iter().collect(),
            pin_phc: Some(hash_of(PIN)),
        },
    );
    staff.insert(
        WAITER_CODE,
        StaffAuth {
            employee_id: Some(EmployeeId::new(Ulid::from_u128(12))),
            permissions: PermissionSet::default(),
            pin_phc: Some(hash_of(PIN)),
        },
    );
    let edge = Arc::new(
        Edge::new(
            FakeStore::default(),
            identity,
            EdgeSession::bootstrap().with_staff(staff),
            Arc::new(InMemoryReceipts::new()),
        )
        .expect("seed"),
    );
    let pairing = Arc::new(Pairing::new());
    let now = SystemClock.now();
    let mut tokens = Vec::new();
    for _ in 0..2 {
        let code = pairing.mint(now).expect("mint a pairing code");
        tokens.push(
            pairing
                .redeem(&code, now)
                .await
                .expect("redeem")
                .token()
                .expect("a fresh code pairs a device")
                .as_str()
                .to_owned(),
        );
    }
    let router = pos_edge::http::domain_router(
        edge,
        InMemoryQueueNumbers::new(),
        pos_edge::print_agent::InMemoryPrintAgents::new(),
        pairing,
        Arc::new(Sessions::new()),
        &Arc::new(pos_edge::origins::Origins::new()),
    );
    let holder = tokens.remove(0);
    let other = tokens.remove(0);
    (router, holder, other)
}

async fn sign_in(app: &Router, token: &str, code: &str) {
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/session/sign-in")
                .header("authorization", format!("Bearer {token}"))
                .header("content-type", "application/json")
                .body(Body::from(json!({ "code": code, "pin": PIN }).to_string()))
                .expect("request builds"),
        )
        .await
        .expect("route the sign-in");
    assert_eq!(response.status(), StatusCode::OK, "the seeded PIN signs in");
}

/// Posts to one of the two agent routes, returning the status and body.
async fn post_agent(app: &Router, token: &str, path: &str, agent: &str) -> (StatusCode, String) {
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(path)
                .header("authorization", format!("Bearer {token}"))
                .header("content-type", "application/json")
                .body(Body::from(json!({ "agent_device_id": agent }).to_string()))
                .expect("request builds"),
        )
        .await
        .expect("route the request");
    let status = response.status();
    let body = response
        .into_body()
        .collect()
        .await
        .expect("read the body")
        .to_bytes();
    (status, String::from_utf8_lossy(&body).into_owned())
}

/// A manager at the box binds a terminal, and a second box cannot take it over.
#[tokio::test]
async fn a_manager_binds_a_terminal_and_a_second_device_is_refused() {
    let (app, first, second) = paired_pair().await;
    sign_in(&app, &first, MANAGER_CODE).await;
    sign_in(&app, &second, MANAGER_CODE).await;

    let (status, body) = post_agent(&app, &first, "/api/print/agent", TILL).await;
    assert_eq!(status, StatusCode::OK);
    assert!(body.contains("BOUND"), "the first claim binds: {body}");

    // Take-over-by-latest is the tempting simplification and it is wrong: two boxes holding one
    // identity both claim from the same queue, so each ticket prints once — on whichever grabbed it.
    let (status, body) = post_agent(&app, &second, "/api/print/agent", TILL).await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        body.contains("HELD_BY_ANOTHER_DEVICE"),
        "the second box is refused, not silently promoted: {body}"
    );

    // The holder releases it, and then the second box may take it — how a dead terminal is replaced.
    let (status, _) = post_agent(&app, &first, "/api/print/agent/revoke", TILL).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let (status, body) = post_agent(&app, &second, "/api/print/agent", TILL).await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        body.contains("BOUND"),
        "the replacement machine binds: {body}"
    );
}

/// A signed-in waiter cannot move where the kitchen's tickets print.
///
/// The sharper of the two gates. A paired device is any box the store admitted, and every waiter's
/// tablet is one; without this check a tap on the wrong screen would redirect a station's tickets to
/// a machine in another room.
#[tokio::test]
async fn a_signed_in_person_without_manage_devices_is_refused() {
    let (app, token, _second) = paired_pair().await;
    sign_in(&app, &token, WAITER_CODE).await;

    let (status, _) = post_agent(&app, &token, "/api/print/agent", TILL).await;
    assert_eq!(
        status,
        StatusCode::FORBIDDEN,
        "binding a terminal takes a manager, not merely a signed-in person"
    );
    let (status, _) = post_agent(&app, &token, "/api/print/agent/revoke", TILL).await;
    assert_eq!(
        status,
        StatusCode::FORBIDDEN,
        "and releasing one takes the same standing as binding it"
    );
}

/// A paired device with nobody signed in is refused before the permission is even considered.
#[tokio::test]
async fn a_paired_device_with_nobody_signed_in_is_refused() {
    let (app, token, _second) = paired_pair().await;
    let (status, _) = post_agent(&app, &token, "/api/print/agent", TILL).await;
    assert_eq!(
        status,
        StatusCode::FORBIDDEN,
        "the signed-in gate answers 403 so the till shows the sign-in screen"
    );
}

/// An unpaired caller does not reach the routes at all.
#[tokio::test]
async fn an_unpaired_caller_is_refused() {
    let (app, _first, _second) = paired_pair().await;
    let (status, _) = post_agent(&app, "not-a-token", "/api/print/agent", TILL).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

/// An id that is not a ULID is a request fault, not a store fault.
#[tokio::test]
async fn an_agent_id_that_is_not_a_ulid_is_refused() {
    let (app, token, _second) = paired_pair().await;
    sign_in(&app, &token, MANAGER_CODE).await;
    let (status, _) = post_agent(&app, &token, "/api/print/agent", "not-a-ulid").await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}
