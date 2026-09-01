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
pub mod auth;
pub mod bills;
pub mod floor;
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

use pos_ports::event_store::EventStore;
use pos_proto::ulid::Ulid;

use crate::app::{AppError, Edge};
use crate::auth::{Lockout, Sessions};
use crate::pairing::Pairing;
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
/// (ADR-0013). Merged with [`router`] at composition.
///
/// Two auth gates guard the surface (ADR-0084), and the router is split so each carries the right
/// one:
/// - **Guarded** — the floor, order, bill, shift and KDS routes — needs a paired device *and* an
///   employee signed in on it, so every read and command runs under a real
///   [`Actor`](pos_core::decision::Actor). It carries both middlewares.
/// - **Session** — sign-in, sign-out, and "who is signed in" — needs a paired device but *not* a
///   sign-in (signing in is how a device passes the second gate). It carries only the first.
///
/// The signed-in bindings ([`Sessions`]) and the PIN lockout ([`Lockout`]) are created here and shared
/// between the session routes (which write them) and the sign-in gate (which reads them); an edge
/// restart clears them, the same in-memory lifetime as the pairing tokens (ADR-0084).
pub fn domain_router<S>(edge: Arc<Edge<S>>, pairing: Arc<Pairing>) -> Router
where
    S: EventStore + Send + Sync + 'static,
{
    let sessions = Arc::new(Sessions::new());
    let lockout = Arc::new(Lockout::new());

    // Guarded: a paired, signed-in device. The signed-in gate is layered here (inner); the paired
    // gate is layered on the merged router below (outer), so it runs first and leaves the `DeviceId`
    // the signed-in gate reads.
    let guarded = Router::new()
        // The store's published floor plan + kitchen stations, for the UI to render real tables and
        // route fires (ADR-0072).
        .route("/api/floor", get(floor::plan::<S>))
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
        .layer(axum::middleware::from_fn_with_state(
            Arc::clone(&sessions),
            auth::require_signed_in,
        ))
        .with_state(Arc::clone(&edge));

    // Session: a paired device signs a person in and out here, so these sit behind the paired gate but
    // not the signed-in one.
    let session = Router::new()
        .route("/api/session", get(auth::current::<S>))
        .route("/api/session/sign-in", post(auth::sign_in::<S>))
        .route("/api/session/sign-out", post(auth::sign_out::<S>))
        .with_state(auth::SignInDeps {
            edge,
            sessions,
            lockout,
        });

    // Every domain route requires a paired device (ADR-0084). The check runs once here, over the
    // pairing state, so it guards reads, writes and the session routes alike before any handler; the
    // middleware carries its own `Arc<Pairing>` state, independent of the routes' own state.
    guarded
        .merge(session)
        .layer(axum::middleware::from_fn_with_state(
            pairing,
            auth::require_paired_device,
        ))
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
        | AppError::UnroutableLine
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
