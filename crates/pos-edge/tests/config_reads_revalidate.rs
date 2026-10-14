// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The till's configuration reads answer a revalidation with a `304`
//! ([ADR-0173](../../../docs/adr/0173-the-till-declares-each-piece-of-store-data-once.md)
//! decision 2).
//!
//! Each of the five reads tags its body, and the same tag sent back in `If-None-Match` is answered
//! with no body. The guard still runs first: a revalidation from a device that is not paired is
//! refused like any other read. Driven with `tower::ServiceExt::oneshot`, like the other route
//! suites: no socket, no race.

use std::sync::Arc;

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use pos_core::permission::PermissionSet;
use pos_edge::pairing::Minter;
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

/// A fixed 16-byte salt, so a hashed fixture is deterministic. Never used in production.
const SALT: &[u8] = b"a-fixed-test-slt";

const STAFF_CODE: &str = "C01";
const STAFF_PIN: &str = "2468";

/// A real Argon2id PHC hash of `pin`, with a fixed salt so the test needs no RNG.
fn hash_of(pin: &str) -> String {
    use argon2::Argon2;
    use argon2::password_hash::PasswordHasher as _;
    Argon2::default()
        .hash_password_with_salt(pin.as_bytes(), SALT)
        .expect("hash")
        .to_string()
}

/// The domain router over the bootstrap session, with the seeded staff signed in.
async fn app() -> (Router, String) {
    let mut roster = StaffRoster::new();
    roster.insert(
        STAFF_CODE,
        StaffAuth {
            employee_id: Some(EmployeeId::new(Ulid::from_u128(11))),
            permissions: PermissionSet::default(),
            permissions_with_approval: PermissionSet::EMPTY,
            discount_ceiling: None,
            pin_phc: Some(hash_of(STAFF_PIN)),
        },
    );
    let session = EdgeSession::bootstrap().with_staff(roster);
    let edge = Arc::new(
        Edge::new(
            FakeStore::default(),
            StoreIdentity::for_store(StoreId::new(Ulid::from_u128(3))),
            session,
            Arc::new(InMemoryReceipts::new()),
        )
        .expect("seed"),
    );
    let pairing = Arc::new(Pairing::new());
    let now = SystemClock.now();
    let (code, _) = pairing
        .mint(now, Minter::Boot)
        .expect("mint a pairing code");
    let token = pairing
        .redeem(&code, now)
        .await
        .expect("redeem")
        .token()
        .expect("a fresh code pairs a device")
        .as_str()
        .to_owned();
    let served = pos_edge::http::domain_router(
        edge,
        InMemoryQueueNumbers::new(),
        Arc::new(pos_edge::print_agent::InMemoryPrintAgents::new()),
        pos_edge::print_queue::InMemoryPrintQueue::new(),
        pos_edge::print_wake::SharedPrintWake::new(),
        pairing,
        Arc::new(Sessions::new()),
        &Arc::new(pos_edge::origins::Origins::new()),
    );
    let signed_in = served
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/session/sign-in")
                .header("authorization", format!("Bearer {token}"))
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({ "code": STAFF_CODE, "pin": STAFF_PIN }).to_string(),
                ))
                .expect("request builds"),
        )
        .await
        .expect("router responds");
    assert_eq!(
        signed_in.status(),
        StatusCode::OK,
        "the seeded staff signs in"
    );
    (served, token)
}

/// The five configuration reads the till revalidates.
const READS: [&str; 5] = [
    "/api/floor",
    "/api/menu",
    "/api/layout",
    "/api/locale",
    "/api/reason-codes",
];

async fn get(
    app: &Router,
    path: &str,
    token: Option<&str>,
    headers: &[(&str, &str)],
) -> axum::response::Response {
    let mut request = Request::builder().uri(path);
    if let Some(token) = token {
        request = request.header("authorization", format!("Bearer {token}"));
    }
    for (name, value) in headers {
        request = request.header(*name, *value);
    }
    app.clone()
        .oneshot(request.body(Body::empty()).expect("request builds"))
        .await
        .expect("router responds")
}

#[tokio::test]
async fn each_configuration_read_is_tagged_and_a_revalidation_gets_a_bodiless_304() {
    let (app, token) = app().await;
    for path in READS {
        let first = get(&app, path, Some(&token), &[]).await;
        assert_eq!(first.status(), StatusCode::OK, "{path}");
        let tag = first.headers()["etag"].to_str().expect("ascii").to_owned();
        assert!(tag.starts_with("W/\""), "{path}: a weak tag, {tag}");
        assert_eq!(
            first.headers()["cache-control"],
            "private, no-cache",
            "{path}"
        );

        let again = get(&app, path, Some(&token), &[("if-none-match", &tag)]).await;
        assert_eq!(again.status(), StatusCode::NOT_MODIFIED, "{path}");
        assert_eq!(
            again.headers()["etag"].to_str().expect("ascii"),
            tag,
            "{path}"
        );
        let body = axum::body::to_bytes(again.into_body(), 1024)
            .await
            .expect("body");
        assert!(body.is_empty(), "{path}: a 304 carries no body");
    }
}

#[tokio::test]
async fn a_tag_that_no_longer_matches_gets_the_whole_body() {
    let (app, token) = app().await;
    let response = get(
        &app,
        "/api/menu",
        Some(&token),
        &[("if-none-match", "W/\"stale\"")],
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
}

#[tokio::test]
async fn a_gzip_client_revalidates_against_the_same_tag() {
    let (app, token) = app().await;
    let plain = get(&app, "/api/menu", Some(&token), &[]).await;
    let tag = plain.headers()["etag"].to_str().expect("ascii").to_owned();
    let gzip = get(
        &app,
        "/api/menu",
        Some(&token),
        &[("accept-encoding", "gzip"), ("if-none-match", &tag)],
    )
    .await;
    assert_eq!(gzip.status(), StatusCode::NOT_MODIFIED);
}

#[tokio::test]
async fn a_revalidation_from_an_unpaired_device_is_refused_first() {
    let (app, _) = app().await;
    for path in READS {
        let response = get(&app, path, None, &[("if-none-match", "*")]).await;
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED, "{path}");
        assert!(response.headers().get("etag").is_none(), "{path}");
    }
}
