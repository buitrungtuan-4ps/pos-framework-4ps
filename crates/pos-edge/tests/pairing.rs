// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! `POST /api/pair` redeems a code for a device token (ADR-0030), driven without a socket.

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use http_body_util::BodyExt;
use pos_edge::{AppState, EdgeConfig};
use pos_proto::ClockSource;
use pos_proto::ids::StoreId;
use pos_proto::ulid::Ulid;
use tower::ServiceExt;

/// A router plus the pairing code minted into its shared state, so a test can redeem it.
fn app_with_code() -> (Router, String) {
    let store = StoreId::new(Ulid::from_u128(1));
    let config = EdgeConfig::new("127.0.0.1:0".parse().expect("addr"), store);
    let state = AppState::new(config);
    let code = state
        .pairing
        .mint(state.clock.now())
        .expect("mint a code")
        .as_str()
        .to_owned();
    (pos_edge::http::router(state.clone()), code)
}

async fn post_pair(app: Router, body: &str) -> (StatusCode, String) {
    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/pair")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(body.to_owned()))
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
async fn a_valid_code_pairs_a_device() {
    let (app, code) = app_with_code();
    let (status, body) = post_pair(app, &format!("{{\"code\":\"{code}\"}}")).await;
    assert_eq!(status, StatusCode::OK);
    let value: serde_json::Value = serde_json::from_str(&body).expect("json");
    let token = value["device_token"].as_str().expect("a token");
    assert_eq!(token.len(), 32, "a device token is 128 bits of hex");
}

#[tokio::test]
async fn a_wrong_code_is_forbidden() {
    let (app, code) = app_with_code();
    // A well-formed but wrong six-digit code — flip the first digit.
    let wrong: String = code
        .chars()
        .enumerate()
        .map(|(i, c)| {
            if i == 0 {
                if c == '0' { '1' } else { '0' }
            } else {
                c
            }
        })
        .collect();
    let (status, _) = post_pair(app, &format!("{{\"code\":\"{wrong}\"}}")).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn a_malformed_code_is_a_bad_request() {
    let (app, _code) = app_with_code();
    let (status, _) = post_pair(app, "{\"code\":\"abc\"}").await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn a_code_pairs_only_once() {
    let (app, code) = app_with_code();
    let body = format!("{{\"code\":\"{code}\"}}");
    let (first, _) = post_pair(app.clone(), &body).await;
    assert_eq!(first, StatusCode::OK);
    // The same code, now spent, is refused.
    let (second, _) = post_pair(app, &body).await;
    assert_eq!(second, StatusCode::FORBIDDEN);
}
