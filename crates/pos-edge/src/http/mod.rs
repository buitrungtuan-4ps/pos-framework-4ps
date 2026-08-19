// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The axum router.
//!
//! Today it answers the health probe and serves the embedded UI. The domain routes (open a table,
//! add a line, settle a bill) and the WebSocket fan-out land in later P5 slices behind the same
//! router ([ADR-0018](../../../docs/adr/0018-http-websocket-stack.md)); each is a synchronous
//! `pos_core` decision applied inside one transaction, with the result published to every connected
//! device.

pub mod assets;
pub mod health;
pub mod pair;
pub mod tables;
pub mod ws;

use std::sync::Arc;

use axum::Router;
use axum::routing::{get, post};
use tower_http::trace::TraceLayer;

use pos_ports::event_store::EventStore;

use crate::app::Edge;
use crate::state::AppState;

/// Builds the router over the shared [`AppState`].
///
/// Kept separate from binding a socket so a test can drive it with
/// [`tower::ServiceExt::oneshot`](https://docs.rs/tower/latest/tower/trait.ServiceExt.html) and never
/// touch the network — the same reason the logic lives in the library, not in `main`.
pub fn router(state: AppState) -> Router {
    Router::new()
        .route("/healthz", get(health::healthz))
        // One WebSocket per device, fed by the fan-out (ADR-0018).
        .route("/ws", get(ws::handler))
        // Redeem a pairing code for a device token (ADR-0030). The human-facing `/pair?code=` URL is
        // a GET that falls through to the single-page app, which posts the code here.
        .route("/api/pair", post(pair::pair))
        // Anything not matched is a UI asset; an unknown path falls back to index.html so a
        // client-routed path (the P6 single-page app) still loads.
        .fallback(assets::serve)
        // Records a span per request; it logs the method, path and status — never a request body,
        // which is where PII would be (see `crate::telemetry`).
        .layer(TraceLayer::new_for_http())
        .with_state(state)
}

/// Builds the domain routes over the application [`Edge`].
///
/// Generic over the store `S`, so the identical routes run against `pos-fakes` and `store-sqlite`
/// (ADR-0013). Merged with [`router`] at composition; its state is the shared `Arc<Edge<S>>`.
pub fn domain_router<S>(edge: Arc<Edge<S>>) -> Router
where
    S: EventStore + Send + Sync + 'static,
{
    Router::new()
        .route("/api/tables/{id}/seat", post(tables::seat::<S>))
        .route("/api/tables/{id}/clean", post(tables::clean::<S>))
        .route("/api/tables/{id}", get(tables::get::<S>))
        .with_state(edge)
}
