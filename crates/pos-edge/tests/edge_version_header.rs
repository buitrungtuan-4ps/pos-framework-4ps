// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The edge stamps its release — and, since
//! [ADR-0123](../../docs/adr/0123-a-superseded-box-opens-nothing-new.md), its lease standing — on
//! every `/api/*` answer ([ADR-0111](../../docs/adr/0111-a-second-origin-may-address-the-edge.md)).
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

use std::sync::Arc;

use axum::Router;
use axum::body::Body;
use axum::http::{Method, Request, StatusCode, header};
use pos_edge::{
    AppState, Edge, EdgeConfig, EdgeSession, InMemoryQueueNumbers, InMemoryReceipts, Pairing,
    Sessions, StoreIdentity,
};
use pos_fakes::FakeStore;
use pos_proto::ids::StoreId;
use pos_proto::ulid::Ulid;
use tower::ServiceExt;

/// The host every request in this file is addressed to — the edge's own serving origin.
const HOST: &str = "till.local";

/// The header this file is about.
const EDGE_VERSION: &str = "pos-edge-version";

/// Its sibling: whether a replacement machine has taken this store (ADR-0123). Same rail, same
/// scope, same CORS exposure, and for the same reason — a supersession, like a version drift,
/// happens *after* pairing.
const LEASE_STANDING: &str = "pos-lease-standing";

/// The application as [`serve`](pos_edge::serve) composes it: the infra router, the domain router
/// merged into it, and the stamp applied **last**.
///
/// Composed rather than taken from `http::router` alone, and that detail is the whole reason this
/// file was rewritten. The earlier version built `http::router(state)` by itself, which does not
/// register `/api/session` at all: the request fell through to the asset fallback, came back
/// `200 text/html`, and the header assertion passed on a response that had nothing to do with the
/// route it named. It was indistinguishable from the `/api/floorplan` fallback test below it, and
/// it reported the layer as working while forty-two of forty-five routes went unstamped in the
/// binary an operator actually runs.
///
/// So: every route asserted here is a route that exists, and every test asserts the **status** as
/// well as the header. A fallback cannot stand in for a domain route again without the status
/// giving it away.
fn app(published: &[&str]) -> Router {
    let store = StoreId::new(Ulid::from_u128(1));
    let config = EdgeConfig::new("127.0.0.1:0".parse().expect("valid addr"), store);
    let pairing = Arc::new(Pairing::new());
    let state = AppState::new(config).with_pairing(Arc::clone(&pairing));
    state
        .origins
        .replace(published)
        .expect("the test's origins are valid");
    // Taken before `router` consumes the state, exactly as `serve` does.
    let standing = Arc::clone(&state.standing);
    let origins = Arc::clone(&state.origins);
    let edge = Arc::new(
        Edge::new(
            FakeStore::default(),
            StoreIdentity::for_store(store),
            EdgeSession::bootstrap(),
            Arc::new(InMemoryReceipts::new()),
        )
        .expect("edge composes"),
    );
    let app = pos_edge::http::router(state).merge(pos_edge::http::domain_router(
        edge,
        InMemoryQueueNumbers::new(),
        Arc::new(pos_edge::print_agent::InMemoryPrintAgents::new()),
        pos_edge::print_queue::InMemoryPrintQueue::new(),
        pos_edge::print_wake::SharedPrintWake::new(),
        pairing,
        Arc::new(Sessions::new()),
        &origins,
    ));
    pos_edge::http::stamp_version(app, standing)
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
/// `/api/session` refuses an unpaired device, and that is the point twice over: the header is on the
/// refusal too — the call that fails is exactly the call an operator is looking at when they ask
/// which version this box is running — and the `401` proves the request reached the real route
/// rather than the asset fallback. Assert the status first; without it this test cannot tell a
/// registered route from a missing one.
#[tokio::test]
async fn an_api_response_carries_the_running_release() {
    let response = app(&[])
        .oneshot(get("/api/session"))
        .await
        .expect("the router answers");
    assert_eq!(
        response.status(),
        StatusCode::UNAUTHORIZED,
        "the premise: /api/session is a registered route that refuses an unpaired device, \
         not a path falling through to the asset fallback",
    );
    assert_eq!(
        stamped(response.headers()).as_deref(),
        Some(pos_edge::version::VERSION),
        "an /api response must name the release that answered it",
    );
}

/// A route merged in after the infra router carries it too.
///
/// This is the case that was broken and that nothing caught. `Router::layer` wraps only the routes
/// registered when it is called, so while the stamp was applied inside `http::router` it reached the
/// three pairing routes and the asset fallback and missed every domain route — forty of the
/// forty-five an edge publishes, including all of these. One route from the merged surface is
/// enough to pin the composition order; `routes_snapshot.rs` owns the full list.
#[tokio::test]
async fn a_route_merged_after_the_infra_router_is_stamped_too() {
    for uri in ["/api/menu", "/api/floor", "/api/orders/open"] {
        let response = app(&[])
            .oneshot(get(uri))
            .await
            .expect("the router answers");
        assert_eq!(
            response.status(),
            StatusCode::UNAUTHORIZED,
            "{uri} must be a registered domain route that refuses an unpaired device",
        );
        assert_eq!(
            stamped(response.headers()).as_deref(),
            Some(pos_edge::version::VERSION),
            "{uri} is merged after the infra router, and a layer applied before the merge \
             would silently miss it",
        );
        assert_eq!(
            response
                .headers()
                .get(LEASE_STANDING)
                .and_then(|value| value.to_str().ok()),
            Some("active"),
            "{uri} must carry the lease standing for the same reason",
        );
    }
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
    assert!(
        exposed.contains(LEASE_STANDING),
        "and to read {LEASE_STANDING}, or a hosted till never learns it has been replaced; got {exposed:?}",
    );
}

/// Every `/api/*` answer also says whether a replacement has taken this store (ADR-0123).
///
/// `active` here, because the test router's `AppState` holds a fresh standing and no lease has been
/// issued — which is every store in the fleet today. What is pinned is that the header is *present*
/// on an ordinary answer: the till's banner reads it off calls it was making anyway, so a header
/// that only appeared on some routes would mean a superseded box that looked fine on whichever
/// screen the operator happened to be on.
#[tokio::test]
async fn an_api_response_says_whether_this_box_is_still_the_store() {
    let response = app(&[])
        .oneshot(get("/api/session"))
        .await
        .expect("the router answers");
    assert_eq!(
        response.status(),
        StatusCode::UNAUTHORIZED,
        "the premise, again: a real route answered this, not the fallback",
    );
    assert_eq!(
        response
            .headers()
            .get(LEASE_STANDING)
            .and_then(|value| value.to_str().ok()),
        Some("active"),
        "a store that has never been issued a lease is active, exactly as before ADR-0123",
    );
}

/// And `/healthz` does not carry it, for the same reason it carries no version.
#[tokio::test]
async fn the_liveness_probe_is_not_told_the_lease_standing_either() {
    let response = app(&[])
        .oneshot(get("/healthz"))
        .await
        .expect("the router answers");
    assert!(
        response.headers().get(LEASE_STANDING).is_none(),
        "/healthz is a service manager's probe, not a till",
    );
}
