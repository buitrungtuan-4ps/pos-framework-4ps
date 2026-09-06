// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! A second origin may address the edge — and only the ones a store published
//! ([ADR-0111](../../docs/adr/0111-a-second-origin-may-address-the-edge.md)).
//!
//! [`crate::origins`](pos_edge::origins) unit-tests the *list*: what validates, what is refused, what
//! survives a refusal. These tests are one level up and answer the question the list cannot: does a
//! request from another origin actually get through, and does a request from an unpublished one
//! actually not. An allow-list that no route consults would pass every unit test in that module and
//! change nothing about the edge.
//!
//! Driven with `tower::ServiceExt::oneshot`, so no socket is bound and no browser is involved: CORS
//! is a set of response headers, and a header is a value a test can read.

use std::sync::Arc;

use axum::Router;
use axum::body::Body;
use axum::http::{HeaderValue, Method, Request, StatusCode, header};
use pos_edge::{
    AppState, Edge, EdgeConfig, EdgeSession, InMemoryQueueNumbers, InMemoryReceipts, Pairing,
    Sessions, StoreIdentity,
};
use pos_fakes::FakeStore;
use pos_proto::ids::StoreId;
use pos_proto::ulid::Ulid;
use tower::ServiceExt;

/// The host every request in this file is addressed to — the edge's own serving origin.
const HOST: &str = "till.local:8080";

/// An origin a store published, reached by a name the box does not serve itself.
const PUBLISHED: &str = "https://shell.example.com";

/// An origin nobody published.
const STRANGER: &str = "https://evil.test";

/// A router whose store has published `origins`, or none if the slice is empty.
fn app(published: &[&str]) -> Router {
    let store = StoreId::new(Ulid::from_u128(1));
    let config = EdgeConfig::new("127.0.0.1:0".parse().expect("valid addr"), store);
    let state = AppState::new(config);
    state
        .origins
        .replace(published)
        .expect("the test's origins are valid");
    pos_edge::http::router(state)
}

/// A CORS preflight for the shape the typed client sends: `POST` with a bearer.
fn preflight(uri: &str, origin: &str) -> Request<Body> {
    Request::builder()
        .method(Method::OPTIONS)
        .uri(uri)
        .header(header::HOST, HOST)
        .header(header::ORIGIN, origin)
        .header(header::ACCESS_CONTROL_REQUEST_METHOD, "POST")
        .header(header::ACCESS_CONTROL_REQUEST_HEADERS, "authorization")
        .body(Body::empty())
        .expect("request builds")
}

/// The `Access-Control-Allow-Origin` a response carries, if any.
fn allow_origin(headers: &axum::http::HeaderMap) -> Option<String> {
    headers
        .get(header::ACCESS_CONTROL_ALLOW_ORIGIN)
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned)
}

#[tokio::test]
async fn a_published_origin_is_allowed_to_pair() {
    let response = app(&[PUBLISHED])
        .oneshot(preflight("/api/pair", PUBLISHED))
        .await
        .expect("router responds");

    assert_eq!(
        allow_origin(response.headers()).as_deref(),
        Some(PUBLISHED),
        "the preflight should echo the single matched origin"
    );
    // The `Authorization` header is what forces the preflight in the first place; a policy that
    // allowed the origin but not the header would preflight-fail every real call.
    let allowed_headers = response
        .headers()
        .get(header::ACCESS_CONTROL_ALLOW_HEADERS)
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default()
        .to_ascii_lowercase();
    assert!(
        allowed_headers.contains("authorization"),
        "the bearer header must be allowed, got {allowed_headers:?}"
    );
    // Ten minutes, the whole of what the estate's browsers honour. Without it a hosted placement
    // pays two WAN round trips per call.
    assert_eq!(
        response
            .headers()
            .get(header::ACCESS_CONTROL_MAX_AGE)
            .and_then(|value| value.to_str().ok()),
        Some("600")
    );
}

#[tokio::test]
async fn an_unpublished_origin_is_not_allowed_to_pair() {
    let response = app(&[PUBLISHED])
        .oneshot(preflight("/api/pair", STRANGER))
        .await
        .expect("router responds");

    // No `Access-Control-Allow-Origin` at all: the browser refuses the call itself. A `403` would be
    // the wrong shape — CORS is enforced by the client against a *missing* header, not by a status.
    assert_eq!(allow_origin(response.headers()), None);
}

#[tokio::test]
async fn the_origin_that_served_the_page_needs_no_list_at_all() {
    // The store that has published nothing — which is nearly all of them, and every store before
    // ADR-0111. Its own UI must keep working, so same-origin is decided against the request's `Host`
    // and never against the list.
    let response = app(&[])
        .oneshot(preflight("/api/pair", &format!("http://{HOST}")))
        .await
        .expect("router responds");

    assert_eq!(
        allow_origin(response.headers()).as_deref(),
        Some(format!("http://{HOST}").as_str())
    );

    // And the scheme is deliberately not compared: behind a TLS-terminating proxy the browser sends
    // `https` while the edge sees `http`, and refusing that would break exactly the hosted
    // deployment ADR-0110 added.
    let response = app(&[])
        .oneshot(preflight("/api/pair", &format!("https://{HOST}")))
        .await
        .expect("router responds");
    assert_eq!(
        allow_origin(response.headers()).as_deref(),
        Some(format!("https://{HOST}").as_str())
    );
}

#[tokio::test]
async fn no_response_ever_grants_credentials() {
    // Absent, not `false`. The device token is a bearer in `Authorization` and no cookie exists
    // anywhere on the edge; turning credentials on would let every allow-listed origin drive the
    // till with the operator's ambient authority, and would hand a cross-site form post the same.
    for origin in [PUBLISHED, &format!("http://{HOST}")] {
        let response = app(&[PUBLISHED])
            .oneshot(preflight("/api/pair", origin))
            .await
            .expect("router responds");
        assert!(
            response
                .headers()
                .get(header::ACCESS_CONTROL_ALLOW_CREDENTIALS)
                .is_none(),
            "{origin} was granted credentials"
        );
        // Vary: Origin, so an intermediary cannot cache one origin's answer and serve it to another
        // — the classic defect in a hand-rolled CORS layer.
        let vary = response
            .headers()
            .get_all(header::VARY)
            .iter()
            .filter_map(|value| value.to_str().ok())
            .collect::<Vec<_>>()
            .join(", ")
            .to_ascii_lowercase();
        assert!(
            vary.contains("origin"),
            "missing Vary: Origin, got {vary:?}"
        );
    }
}

#[tokio::test]
async fn a_route_no_constructor_named_is_not_covered() {
    // `/healthz` answers an unauthenticated probe and carries no data; ADR-0111 declares it not
    // covered, and this pins that the layer is applied to named subsets rather than to the merged
    // application. If a later refactor layers CORS at the top, this fails.
    let response = app(&[PUBLISHED])
        .oneshot(
            Request::builder()
                .uri("/healthz")
                .header(header::HOST, HOST)
                .header(header::ORIGIN, PUBLISHED)
                .body(Body::empty())
                .expect("request builds"),
        )
        .await
        .expect("router responds");

    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(allow_origin(response.headers()), None);
}

/// A `GET /ws` upgrade request from `origin`.
///
/// Carries `Connection`/`Upgrade` and nothing else of the handshake. `Sec-WebSocket-Key` and
/// `Sec-WebSocket-Version` are deliberately absent: both outcomes these tests assert are decided in
/// *middleware* — the origin gate, then the device-token gate — and neither reaches axum's
/// `WebSocketUpgrade` extractor, which is the only thing that would read them. Including them would
/// add nothing but a nonce with the shape of a credential, which the repository's secret scanner
/// reads as one (it is RFC 6455 §1.3's published example, and it is not a secret — but a test that
/// needs no key should not carry one).
fn upgrade(origin: Option<&str>) -> Request<Body> {
    let mut request = Request::builder()
        .uri("/ws")
        .header(header::HOST, HOST)
        .header(header::CONNECTION, "Upgrade")
        .header(header::UPGRADE, "websocket")
        .body(Body::empty())
        .expect("request builds");
    if let Some(origin) = origin {
        request.headers_mut().insert(
            header::ORIGIN,
            HeaderValue::from_str(origin).expect("a valid header value"),
        );
    }
    request
}

#[tokio::test]
async fn an_unpublished_origin_cannot_open_a_socket() {
    let response = app(&[PUBLISHED])
        .oneshot(upgrade(Some(STRANGER)))
        .await
        .expect("router responds");

    // `403`, not a missing header: a browser applies no same-origin policy to a WebSocket handshake,
    // so nothing on the client side would enforce a CORS answer. The server has to refuse.
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn a_permitted_origin_reaches_the_pairing_gate() {
    // The origin gate is outermost, so "allowed" means it got as far as the *token* check — which
    // these requests then fail, because they carry none. `401` rather than `403` is the proof: a
    // different gate, further in, answered.
    for origin in [
        Some(PUBLISHED),
        Some(&format!("http://{HOST}") as &str),
        // No `Origin` at all is a native shell or a runbook's `websocat`, not a browser. The device
        // token is the gate for those, as it has always been.
        None,
    ] {
        let response = app(&[PUBLISHED])
            .oneshot(upgrade(origin))
            .await
            .expect("router responds");
        assert_eq!(
            response.status(),
            StatusCode::UNAUTHORIZED,
            "{origin:?} should have reached the pairing gate"
        );
    }
}

#[tokio::test]
async fn the_domain_surface_is_covered_and_still_needs_a_paired_device() {
    // Every domain route is covered — ADR-0111's whole point is that a second front-end can sell.
    // The layer is applied outermost, *over* the paired-device gate, because a preflight carries no
    // `Authorization` by specification: a CORS layer inside the gate would answer every preflight
    // `401`, which reads to an operator as "pairing is broken".
    let store = StoreId::new(Ulid::from_u128(1));
    let edge = Arc::new(
        Edge::new(
            FakeStore::default(),
            StoreIdentity::for_store(store),
            EdgeSession::bootstrap(),
            Arc::new(InMemoryReceipts::new()),
        )
        .expect("edge composes"),
    );
    let origins = Arc::new(pos_edge::origins::Origins::new());
    origins
        .replace([PUBLISHED])
        .expect("the test's origins are valid");
    let router = pos_edge::http::domain_router(
        edge,
        InMemoryQueueNumbers::new(),
        Arc::new(pos_edge::print_agent::InMemoryPrintAgents::new()),
        pos_edge::print_queue::InMemoryPrintQueue::new(),
        pos_edge::print_wake::SharedPrintWake::new(),
        Arc::new(Pairing::new()),
        Arc::new(Sessions::new()),
        &origins,
    );

    // The preflight is answered, and answered with the origin — not refused by the gate it sits
    // outside of.
    assert_eq!(
        allow_origin(
            router
                .clone()
                .oneshot(preflight("/api/tables", PUBLISHED))
                .await
                .expect("router responds")
                .headers()
        )
        .as_deref(),
        Some(PUBLISHED)
    );

    // And the gate is still there: the real call behind that preflight carries no device token, so
    // it is refused `401`. A CORS layer is not an authorisation layer, and this pins that adding one
    // did not open the surface it covers.
    let response = router
        .oneshot(
            Request::builder()
                .uri("/api/tables")
                .header(header::HOST, HOST)
                .header(header::ORIGIN, PUBLISHED)
                .body(Body::empty())
                .expect("request builds"),
        )
        .await
        .expect("router responds");
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}
