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
use pos_edge::{Edge, EdgeSession, InMemoryReceipts, StoreIdentity};
use pos_fakes::FakeStore;
use pos_proto::ids::{StoreId, TableId};
use pos_proto::ulid::Ulid;
use tower::ServiceExt;

fn app() -> Router {
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
    pos_edge::http::domain_router(edge)
}

fn table_path(suffix: &str) -> String {
    let table = TableId::new(Ulid::from_u128(100));
    format!("/api/tables/{table}{suffix}")
}

async fn send(app: Router, method: &str, uri: &str) -> (StatusCode, String) {
    let response = app
        .oneshot(
            Request::builder()
                .method(method)
                .uri(uri)
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
    let app = app();
    let (status, body) = send(app.clone(), "POST", &table_path("/seat")).await;
    assert_eq!(status, StatusCode::OK);
    let view: serde_json::Value = serde_json::from_str(&body).expect("json");
    assert_eq!(view["state"], "TABLE_STATE_OCCUPIED");

    // The same router (sharing the same Edge) now reports the table occupied.
    let (status, body) = send(app, "GET", &table_path("")).await;
    assert_eq!(status, StatusCode::OK);
    let view: serde_json::Value = serde_json::from_str(&body).expect("json");
    assert_eq!(view["state"], "TABLE_STATE_OCCUPIED");
}

#[tokio::test]
async fn an_illegal_move_is_a_conflict_not_a_server_error() {
    // Cleaning a fresh (free) table is not a legal transition.
    let (status, _) = send(app(), "POST", &table_path("/clean")).await;
    assert_eq!(status, StatusCode::CONFLICT);
}

#[tokio::test]
async fn a_non_ulid_table_id_is_a_bad_request() {
    let (status, _) = send(app(), "POST", "/api/tables/not-a-ulid/seat").await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}
