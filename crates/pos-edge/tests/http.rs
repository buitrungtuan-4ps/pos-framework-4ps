// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The router answers `/healthz` and serves the embedded UI — proven without binding a socket.
//!
//! The router is driven with `tower::ServiceExt::oneshot`, so these are ordinary async unit tests: no
//! port is bound, nothing races, and the whole file runs in milliseconds. This is the same reason the
//! serving logic lives in the library rather than in `main`.

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use pos_edge::{AppState, EdgeConfig};
use pos_proto::ids::StoreId;
use pos_proto::ulid::Ulid;
use tower::ServiceExt;

/// A router over a fixed dev store, bound to nothing (`:0` is never actually bound here).
fn app() -> Router {
    let store = StoreId::new(Ulid::from_u128(1));
    let config = EdgeConfig::new("127.0.0.1:0".parse().expect("valid addr"), store);
    pos_edge::http::router(AppState::new(config))
}

#[tokio::test]
async fn healthz_reports_ok_and_the_protocol_version() {
    let response = app()
        .oneshot(
            Request::builder()
                .uri("/healthz")
                .body(Body::empty())
                .expect("request builds"),
        )
        .await
        .expect("router responds");

    assert_eq!(response.status(), StatusCode::OK);

    let bytes = response
        .into_body()
        .collect()
        .await
        .expect("body collects")
        .to_bytes();
    let body: serde_json::Value = serde_json::from_slice(&bytes).expect("json body");

    assert_eq!(body["status"], "ok");
    assert_eq!(body["protocol_version"], pos_proto::PROTOCOL_VERSION);
    assert_eq!(body["store_id"], "00000000000000000000000001");
}

#[tokio::test]
async fn the_root_serves_the_embedded_ui() {
    let response = app()
        .oneshot(
            Request::builder()
                .uri("/")
                .body(Body::empty())
                .expect("request builds"),
        )
        .await
        .expect("router responds");

    assert_eq!(response.status(), StatusCode::OK);
    let content_type = response
        .headers()
        .get("content-type")
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default()
        .to_owned();
    assert!(
        content_type.starts_with("text/html"),
        "index.html should be served as HTML, got {content_type:?}"
    );

    let bytes = response
        .into_body()
        .collect()
        .await
        .expect("body collects")
        .to_bytes();
    let html = String::from_utf8(bytes.to_vec()).expect("utf-8 html");
    assert!(html.contains("Pizza 4P"), "the placeholder UI is served");
}

#[tokio::test]
async fn an_unknown_path_falls_back_to_the_single_page_app() {
    // A client-routed path the SPA will resolve — it gets index.html, not a 404.
    let response = app()
        .oneshot(
            Request::builder()
                .uri("/floor/table/7")
                .body(Body::empty())
                .expect("request builds"),
        )
        .await
        .expect("router responds");

    assert_eq!(response.status(), StatusCode::OK);
    let content_type = response
        .headers()
        .get("content-type")
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default()
        .to_owned();
    assert!(content_type.starts_with("text/html"));
}
