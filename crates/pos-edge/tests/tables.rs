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
use pos_edge::{Edge, EdgeSession, InMemoryReceipts, Pairing, StoreIdentity, SystemClock};
use pos_fakes::FakeStore;
use pos_proto::ClockSource;
use pos_proto::ids::{StoreId, TableId};
use pos_proto::ulid::Ulid;
use tower::ServiceExt;

/// The domain router plus a bearer token for a device paired against it — every command route now
/// requires one (ADR-0084).
fn app() -> (Router, String) {
    let identity = StoreIdentity::for_store(StoreId::new(Ulid::from_u128(3)));
    let edge = Arc::new(
        Edge::new(
            FakeStore::default(),
            identity,
            EdgeSession::bootstrap(),
            Arc::new(InMemoryReceipts::new()),
        )
        .expect("seed"),
    );
    let pairing = Arc::new(Pairing::new());
    let now = SystemClock.now();
    let code = pairing.mint(now).expect("mint a pairing code");
    let token = pairing
        .redeem(&code, now)
        .expect("redeem")
        .expect("a fresh code pairs a device")
        .as_str()
        .to_owned();
    (pos_edge::http::domain_router(edge, pairing), token)
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
    let (app, token) = app();
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
    let (app, token) = app();
    let (status, _) = send(app, &token, "POST", &table_path("/clean")).await;
    assert_eq!(status, StatusCode::CONFLICT);
}

#[tokio::test]
async fn a_non_ulid_table_id_is_a_bad_request() {
    let (app, token) = app();
    let (status, _) = send(app, &token, "POST", "/api/tables/not-a-ulid/seat").await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn a_request_without_a_device_token_is_unauthorized() {
    // The gate closes "any host on the store LAN commands the edge" (ADR-0084): a seat with no
    // bearer token is refused before the handler, and a read is refused the same way.
    let (app, _token) = app();
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
