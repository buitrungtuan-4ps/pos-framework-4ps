// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The axum router.
//!
//! Two routers compose the surface ([ADR-0018](../../../docs/adr/0018-http-websocket-stack.md)): an
//! infrastructure [`router`] over [`AppState`] (health, the WebSocket fan-out, pairing, and the
//! embedded UI), and a [`domain_router`] over the application [`Edge`] carrying the floor, order,
//! bill and shift routes. Each domain route is a thin shell: parse the path, call the synchronous
//! `pos_core` decision the [`crate::app`] loop applies inside one transaction, and map the outcome
//! to a status — a refused command is the caller's fault (`409`), an unreachable store is `503`.

pub mod assets;
pub mod bills;
pub mod health;
pub mod kds;
pub mod lines;
pub mod pair;
pub mod shifts;
pub mod tables;
pub mod ws;

use std::str::FromStr;
use std::sync::Arc;

use axum::Router;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use tower_http::trace::TraceLayer;

use pos_core::decision::Actor;
use pos_ports::event_store::EventStore;
use pos_proto::ids::{DeviceId, EmployeeId};
use pos_proto::ulid::Ulid;

use crate::app::{AppError, Edge};
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
        // The floor: seat, clean, read.
        .route("/api/tables/{id}/seat", post(tables::seat::<S>))
        .route("/api/tables/{id}/clean", post(tables::clean::<S>))
        .route("/api/tables/{id}", get(tables::get::<S>))
        // The order: add a line to a table, fire a line to the kitchen.
        .route("/api/tables/{id}/lines", post(lines::add::<S>))
        .route("/api/lines/{id}/fire", post(lines::fire::<S>))
        // The kitchen display: bump a ticket (mark lines prepared), durable and fanned out.
        .route("/api/kds/bump", post(kds::bump::<S>))
        // The bill: open on a table, settle.
        .route("/api/tables/{id}/bill", post(bills::open::<S>))
        .route("/api/bills/{id}/settle", post(bills::settle::<S>))
        // The cash shift: open, blind count, close.
        .route("/api/shifts", post(shifts::open::<S>))
        .route("/api/shifts/{id}/count", post(shifts::count::<S>))
        .route("/api/shifts/{id}/close", post(shifts::close::<S>))
        .with_state(edge)
}

/// Parses a ULID from a path segment. `None` if it is not a ULID, which every handler turns into a
/// [`bad_request`].
pub(crate) fn parse_ulid(id: &str) -> Option<Ulid> {
    Ulid::from_str(id).ok()
}

/// The `400` for a path segment that is not a ULID.
pub(crate) fn bad_request(what: &'static str) -> Response {
    (StatusCode::BAD_REQUEST, what).into_response()
}

/// Maps a refused or failed command to a status — the one place the edge decides which HTTP code a
/// failure kind is, so every domain route answers the same way.
///
/// A refused command (an illegal transition, a missing permission, a disabled capability, or a
/// table/line/bill/shift that is not in a state the command applies to) is the caller's fault, so it
/// is `409 Conflict` rather than `500`. An unreachable store is `503`; a clock or encoding failure is
/// the edge's own `500`.
pub(crate) fn error_response(error: &AppError) -> Response {
    match error {
        AppError::Domain(inner) => (StatusCode::CONFLICT, inner.to_string()).into_response(),
        AppError::NoOpenOrder
        | AppError::UnknownLine
        | AppError::UnknownBill
        | AppError::UnknownShift
        | AppError::ShiftAlreadyOpen => (StatusCode::CONFLICT, error.to_string()).into_response(),
        AppError::Port(_) => {
            (StatusCode::SERVICE_UNAVAILABLE, "the store is unavailable").into_response()
        }
        AppError::Clock | AppError::Encode(_) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            "the edge could not apply the command",
        )
            .into_response(),
    }
}

/// A fixed development actor, until the paired device token and signed-in employee are resolved from
/// the request ([ADR-0030](../../../docs/adr/0030-pairing-and-offline-auth.md)); the auth-integration
/// follow-up replaces it.
pub(crate) fn dev_actor() -> Actor {
    Actor {
        employee_id: EmployeeId::new(Ulid::from_u128(1)),
        device_id: DeviceId::new(Ulid::from_u128(1)),
    }
}
