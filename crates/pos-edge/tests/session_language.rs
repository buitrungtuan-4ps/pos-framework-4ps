// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! `GET /api/session` tells a device nobody is signed in on the store's display language (pilot
//! run 2, N8).
//!
//! `GET /api/locale` answers only a signed-in person, so without this a device that had never
//! signed in drew its sign-in screen in English on a Vietnamese store. The session read sits behind
//! the paired gate alone, and carries the language and nothing else of the `locale` node.
//!
//! Driven with `tower::ServiceExt::oneshot`, like the other route suites: no socket, no race.

use std::sync::Arc;

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use pos_edge::pairing::Minter;
use pos_edge::{
    Edge, EdgeSession, InMemoryQueueNumbers, InMemoryReceipts, Pairing, Sessions, StoreIdentity,
    SystemClock,
};
use pos_fakes::FakeStore;
use pos_proto::ClockSource;
use pos_proto::ids::StoreId;
use pos_proto::ulid::Ulid;
use serde_json::{Value, json};
use tower::ServiceExt;

/// The domain router over a session built by `seed`, and the token of a device paired with it.
/// Nobody signs in.
async fn paired(seed: impl FnOnce(&mut EdgeSession)) -> (Router, String) {
    let mut session = EdgeSession::bootstrap();
    seed(&mut session);
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
    (served, token)
}

/// `GET /api/session` as the paired device, as JSON.
async fn session(app: &Router, token: &str) -> Value {
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/api/session")
                .header("authorization", format!("Bearer {token}"))
                .body(Body::empty())
                .expect("request builds"),
        )
        .await
        .expect("router responds");
    assert_eq!(response.status(), StatusCode::OK, "no sign-in is needed");
    let bytes = response
        .into_body()
        .collect()
        .await
        .expect("body")
        .to_bytes();
    serde_json::from_slice(&bytes).expect("json body")
}

#[tokio::test]
async fn a_device_nobody_signed_in_on_hears_the_stores_language() {
    let (app, token) = paired(|session| {
        session.display_language = Some("vi-VN".to_owned());
    })
    .await;
    let body = session(&app, &token).await;
    assert_eq!(body["signed_in"], json!(false));
    assert_eq!(body["display_language"], json!("vi-VN"));
    assert!(
        body.get("currency_code").is_none(),
        "only the language crosses the gate, not the money settings: {body}"
    );
}

#[tokio::test]
async fn a_store_that_sets_no_language_sends_none() {
    let (app, token) = paired(|_| {}).await;
    let body = session(&app, &token).await;
    assert_eq!(body["signed_in"], json!(false));
    assert!(body.get("display_language").is_none(), "got {body}");
}
