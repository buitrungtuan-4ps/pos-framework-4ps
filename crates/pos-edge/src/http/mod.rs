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

use axum::Router;
use axum::routing::get;
use tower_http::trace::TraceLayer;

use crate::state::AppState;

/// Builds the router over the shared [`AppState`].
///
/// Kept separate from binding a socket so a test can drive it with
/// [`tower::ServiceExt::oneshot`](https://docs.rs/tower/latest/tower/trait.ServiceExt.html) and never
/// touch the network — the same reason the logic lives in the library, not in `main`.
pub fn router(state: AppState) -> Router {
    Router::new()
        .route("/healthz", get(health::healthz))
        // Anything not matched is a UI asset; an unknown path falls back to index.html so a
        // client-routed path (the P6 single-page app) still loads.
        .fallback(assets::serve)
        // Records a span per request; it logs the method, path and status — never a request body,
        // which is where PII would be (see `crate::telemetry`).
        .layer(TraceLayer::new_for_http())
        .with_state(state)
}
