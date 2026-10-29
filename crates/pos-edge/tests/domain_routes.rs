// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! Every published edge route the domain router serves is reachable, and behind the paired gate.
//!
//! The route snapshot (`routes_snapshot.rs`) is read from the source, so it proves what is
//! *registered*, not what the composed router *answers*: a sub-router that is built and never
//! merged keeps its `.route(` lines and passes it. This drives the router itself. Every snapshot
//! route the domain router serves is matched, by its method, for a paired device, and the routes
//! other routers serve are named, so a domain route that stops being served fails here rather than
//! in a store. Without a paired device every one of them is refused by the gate.

use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use pos_edge::pairing::Minter;
use pos_edge::{
    Edge, EdgeSession, InMemoryQueueNumbers, InMemoryReceipts, Pairing, Sessions, StoreIdentity,
    SystemClock,
};
use pos_fakes::FakeStore;
use pos_proto::ClockSource;
use pos_proto::ids::StoreId;
use pos_proto::ulid::Ulid;
use tower::ServiceExt;

/// The snapshot routes the domain router does not serve: the infrastructure router's, the
/// activation router's and pairing's, each registered and composed elsewhere.
const SERVED_ELSEWHERE: &[&str] = &[
    "GET /api/activation",
    "GET /api/claim",
    "GET /api/pair/devices",
    "GET /api/pair/this_device",
    "POST /api/activate",
    "POST /api/pair",
];

/// The snapshot's routes, `METHOD /path`, in its order.
fn snapshot_routes() -> Vec<String> {
    let path =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs/snapshots/routes.txt");
    std::fs::read_to_string(path)
        .expect("the route snapshot")
        .lines()
        .filter(|line| !line.starts_with('#') && !line.trim().is_empty())
        .map(str::to_owned)
        .collect()
}

/// `path` with each `{parameter}` replaced by a well-formed id.
fn concrete(path: &str) -> String {
    path.split('/')
        .map(|segment| {
            if segment.starts_with('{') {
                "01J0000000000000000000000A"
            } else {
                segment
            }
        })
        .collect::<Vec<_>>()
        .join("/")
}

#[tokio::test]
async fn every_domain_route_is_served_behind_the_paired_gate() {
    let edge = Arc::new(
        Edge::new(
            FakeStore::default(),
            StoreIdentity::for_store(StoreId::new(Ulid::from_u128(4))),
            EdgeSession::bootstrap(),
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
    let router = pos_edge::http::domain_router(
        edge,
        InMemoryQueueNumbers::new(),
        Arc::new(pos_edge::print_agent::InMemoryPrintAgents::new()),
        pos_edge::print_queue::InMemoryPrintQueue::new(),
        pos_edge::print_wake::SharedPrintWake::new(),
        pairing,
        Arc::new(Sessions::new()),
        &Arc::new(pos_edge::origins::Origins::new()),
    );
    let mut not_served = Vec::new();
    for route in snapshot_routes() {
        let (method, path) = route.split_once(' ').expect("METHOD /path");
        let response = router
            .clone()
            .oneshot(
                Request::builder()
                    .method(method)
                    .uri(concrete(path))
                    .header("authorization", format!("Bearer {token}"))
                    .body(Body::empty())
                    .expect("request builds"),
            )
            .await
            .expect("router responds");
        let status = response.status();
        if status == StatusCode::NOT_FOUND || status == StatusCode::METHOD_NOT_ALLOWED {
            not_served.push(route.clone());
        }
    }
    not_served.sort();
    assert_eq!(not_served, SERVED_ELSEWHERE);

    // And without a paired device every one of them is refused before any handler runs.
    for route in snapshot_routes() {
        let (method, path) = route.split_once(' ').expect("METHOD /path");
        let response = router
            .clone()
            .oneshot(
                Request::builder()
                    .method(method)
                    .uri(concrete(path))
                    .body(Body::empty())
                    .expect("request builds"),
            )
            .await
            .expect("router responds");
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED, "{route}");
    }
}
