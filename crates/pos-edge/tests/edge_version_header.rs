// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The edge stamps its release on every `/api/*` answer
//! ([ADR-0111](../../docs/adr/0111-a-second-origin-may-address-the-edge.md)).
//!
//! Version drift between an app and the edge it talks to shows up **after** pairing — an OTA ring
//! moves the edge on a Tuesday, or a shell updates itself overnight — so a value read once at
//! pairing time is a value that was true once. A response header rides the answer the app already
//! asked for, and arrives on the call that just failed rather than on a poll whose timing the app
//! would have to guess.
//!
//! Three properties are pinned here, and the third is the one that makes the mechanism worth having:
//! the header is on the **asset fallback's** answer too. A path one side moved does not `404` on this
//! edge — `assets::serve` returns `200 text/html` for anything unmatched — so without the header the
//! app reports a `SyntaxError` from a JSON parse, naming neither the route nor the release.
//!
//! Driven with `tower::ServiceExt::oneshot`: no socket, no browser, and a header is a value a test
//! can read.

use axum::Router;
use axum::body::Body;
use axum::http::{Method, Request, StatusCode, header};
use pos_edge::{AppState, EdgeConfig};
use pos_proto::ids::StoreId;
use pos_proto::ulid::Ulid;
use tower::ServiceExt;

/// The host every request in this file is addressed to — the edge's own serving origin.
const HOST: &str = "till.local";

/// The header this file is about.
const EDGE_VERSION: &str = "pos-edge-version";

/// A router with `published` as this store's origin allow-list.
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

/// A plain same-origin `GET`.
fn get(uri: &str) -> Request<Body> {
    Request::builder()
        .method(Method::GET)
        .uri(uri)
        .header(header::HOST, HOST)
        .body(Body::empty())
        .expect("request builds")
}

/// The `pos-edge-version` a response carries, if any.
fn stamped(headers: &axum::http::HeaderMap) -> Option<String> {
    headers
        .get(EDGE_VERSION)
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned)
}

/// Every `/api/*` answer carries the running release.
///
/// `/api/session` refuses an unpaired device, and that is the point: the header is on the refusal
/// too. The call that fails is exactly the call an operator is looking at when they ask which
/// version this box is running.
#[tokio::test]
async fn an_api_response_carries_the_running_release() {
    let response = app(&[])
        .oneshot(get("/api/session"))
        .await
        .expect("the router answers");
    assert_eq!(
        stamped(response.headers()).as_deref(),
        Some(pos_edge::version::VERSION),
        "an /api response must name the release that answered it",
    );
}

/// The asset fallback's answer carries it too, which is the failure this exists to explain.
///
/// `/api/floorplan` is not a route. It does not `404`: the fallback serves the single-page app, so
/// the app receives `200 text/html` and reports a parse error. With the header that same response
/// says which release it came from, which turns an unattributable `SyntaxError` into "this edge is
/// older than this app".
#[tokio::test]
async fn a_moved_route_still_says_which_release_answered() {
    let response = app(&[])
        .oneshot(get("/api/floorplan"))
        .await
        .expect("the router answers");
    assert_eq!(
        response.status(),
        StatusCode::OK,
        "the premise: an unmatched /api path falls through to the app rather than 404ing",
    );
    assert_eq!(
        stamped(response.headers()).as_deref(),
        Some(pos_edge::version::VERSION),
        "the fallback's answer is the one that most needs the header",
    );
}

/// `/healthz` does not carry it.
///
/// ADR-0111 scopes the header to `/api/*`. `/healthz` serves a service manager's liveness probe and
/// already reports the version in its body; stamping it as well would widen the surface for a caller
/// that never asked.
#[tokio::test]
async fn the_liveness_probe_is_not_stamped() {
    let response = app(&[])
        .oneshot(get("/healthz"))
        .await
        .expect("the router answers");
    assert_eq!(
        stamped(response.headers()),
        None,
        "/healthz is outside the header's scope",
    );
}

/// A cross-origin response says the page may read the header.
///
/// Without `Access-Control-Expose-Headers` a browser hides it from the page, and the whole mechanism
/// silently does nothing for the second origin — which is the only caller that can drift from its
/// edge at all. Same class of omission as a missing `Vary`.
#[tokio::test]
async fn a_cross_origin_response_exposes_the_header_to_the_page() {
    let request = Request::builder()
        .method(Method::GET)
        .uri("/api/pair/devices")
        .header(header::HOST, HOST)
        .header(header::ORIGIN, "https://till.example")
        .body(Body::empty())
        .expect("request builds");
    let response = app(&["https://till.example"])
        .oneshot(request)
        .await
        .expect("the router answers");
    let exposed = response
        .headers()
        .get(header::ACCESS_CONTROL_EXPOSE_HEADERS)
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default()
        .to_ascii_lowercase();
    assert!(
        exposed.contains(EDGE_VERSION),
        "a page on a published origin must be allowed to read {EDGE_VERSION}; got {exposed:?}",
    );
}
