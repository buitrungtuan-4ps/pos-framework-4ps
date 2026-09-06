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
//!
//! The infra router is *mostly* unauthenticated by necessity — a health probe, and the pairing
//! exchange a device needs before it has any credential — with `/ws` the exception: it carries the
//! paired-device gate, because what it streams is the store's committed event log.

pub mod assets;
pub mod auth;
pub mod bills;
pub mod check;
mod counter;
pub mod floor;
pub mod health;
pub mod kds;
pub mod layout;
pub mod lines;
pub mod locale;
pub mod menu;
pub mod pair;
mod print_agent;
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
use pos_ports::subject_store::SubjectStore;
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
    // One WebSocket per device, fed by the fan-out (ADR-0018) — behind the paired-device gate
    // (roadmap-v3 S0c). It is a sub-router precisely so the gate covers `/ws` and nothing else here:
    // `/healthz` must answer an unauthenticated probe, `/api/pair` is how a device *gets* a token,
    // and the asset fallback serves the app that does the pairing.
    //
    // Before S0c this route sat on the ungated router and streamed every committed event — orders,
    // bills, settlements — to any host that could route to the box. ADR-0084 deferred the fix to
    // B6.1; the 2026-09-02 tree audit ruled it a live hole rather than a deferral. The read-only
    // *scope* and the per-event-type filter still belong to B6.1.
    let live = Router::new()
        .route("/ws", get(ws::handler))
        .layer(axum::middleware::from_fn_with_state(
            Arc::clone(&state.pairing),
            auth::require_paired_device_ws,
        ))
        // Outermost, so a cross-origin upgrade is refused before the token is even looked at. `/ws`
        // carries its own origin check rather than the CORS layer the `/api` routes carry, because a
        // browser applies no same-origin policy to a WebSocket handshake at all — the layer would be
        // decoration on this route (ADR-0111, and see `require_permitted_origin_ws`).
        .layer(axum::middleware::from_fn_with_state(
            Arc::clone(&state.origins),
            crate::origins::require_permitted_origin_ws,
        ))
        .with_state(state.clone());

    // One policy value, applied to the covered subsets and to nothing else (ADR-0111). Layering it
    // on the merged application would be the single point that reaches every covered route — and
    // would also cover `/healthz`, `/ws`, the asset fallback and `POST /api/activate`, every one of
    // which that record declares *not* covered. A route is covered because a constructor named it.
    let cors = crate::origins::cors_layer(&state.origins);

    // Redeem a pairing code for a device token (ADR-0030). The human-facing `/pair?code=` URL is a
    // GET that falls through to the single-page app, which posts the code here.
    //
    // Its own sub-router, and not a `.route(…).layer(cors)` on the merged one, because
    // `Router::layer` covers every route added *before* it — which would silently pull in `/healthz`
    // and `/ws`, both of which ADR-0111 declares not covered. A sub-router makes the coverage the
    // shape the record describes instead of an artefact of statement order.
    let pair = Router::new()
        .route("/api/pair", post(pair::pair))
        .layer(cors.clone())
        .with_state(state.clone());

    let state_for_revoke = state.clone();
    Router::new()
        .route("/healthz", get(health::healthz))
        .merge(live)
        .merge(pair)
        // Retiring a device, and reporting how many are paired (ADR-0091). Behind the
        // paired-device gate rather than open: a device that is itself paired can retire another,
        // which is as strong as pairing and no stronger — the edge has no operator identity offline.
        .merge(
            Router::new()
                .route("/api/pair/devices", get(pair::devices))
                .route("/api/pair/revoke", post(pair::revoke))
                .layer(axum::middleware::from_fn_with_state(
                    Arc::clone(&state_for_revoke.pairing),
                    auth::require_paired_device,
                ))
                // Outside the paired gate: a preflight carries no `Authorization` by specification,
                // so a layer applied inside would answer every preflight `401` — and that failure
                // reads to an operator as "pairing is broken", the worst possible mislabelling of a
                // routing mistake.
                .layer(cors)
                .with_state(state_for_revoke),
        )
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
/// The signed-in bindings ([`Sessions`]) are supplied by the caller, because `serve` builds a
/// *durable* one over the store's device registry and loads it before the first request arrives
/// (ADR-0091) — so a restart no longer makes every member of staff re-enter a PIN. A caller with no
/// registry (a test, the on-fakes example) passes `Arc::new(Sessions::new())` and gets the
/// in-memory lifetime this had before S0d. The PIN lockout ([`Lockout`]) is still created here: it
/// is a rate limiter, and a restart clearing it is the safe direction (it forgets failures, never
/// successes).
pub fn domain_router<S, Q, A>(
    edge: Arc<Edge<S>>,
    queue: Q,
    agents: A,
    pairing: Arc<Pairing>,
    sessions: Arc<Sessions>,
    origins: &Arc<crate::origins::Origins>,
) -> Router
where
    S: EventStore + SubjectStore + Send + Sync + 'static,
    Q: crate::queue::QueueNumberAuthority + 'static,
    A: crate::print_agent::PrintAgents + 'static,
{
    let lockout = Arc::new(Lockout::new());
    // Cloned before `edge` and `sessions` move into the routers below.
    let counter_edge = Arc::clone(&edge);
    let agent_edge = Arc::clone(&edge);
    let sessions_for_counter = Arc::clone(&sessions);
    let sessions_for_agents = Arc::clone(&sessions);

    // Guarded: a paired, signed-in device. The signed-in gate is layered here (inner); the paired
    // gate is layered on the merged router below (outer), so it runs first and leaves the `DeviceId`
    // the signed-in gate reads.
    let guarded = Router::new()
        // The store's published floor plan + kitchen stations, for the UI to render real tables and
        // route fires (ADR-0072).
        .route("/api/floor", get(floor::plan::<S>))
        // The store's published price book, so the till prices from what the console published
        // rather than from a list compiled into the app (roadmap-v3 E5, ADR-0063).
        .route("/api/menu", get(menu::catalog::<S>))
        // How the till groups and orders those items, from the `layout` node the same publish writes
        // (ADR-0066, production-readiness C4). A separate node, so a separate route: a price change
        // relays no buttons and a button moving reprices nothing.
        .route("/api/layout", get(layout::plan::<S>))
        // The money facts the pay pad needs — currency, the notes a guest carries, what the total
        // rounds to in cash. A country's coinage, published rather than compiled in (ADR-0105).
        .route("/api/locale", get(locale::settings::<S>))
        // The floor: seat, clean, read.
        .route("/api/tables/{id}/seat", post(tables::seat::<S>))
        .route("/api/tables/{id}/clean", post(tables::clean::<S>))
        .route("/api/tables/{id}", get(tables::get::<S>))
        // What the table owes right now, assembled by the edge — the till displays the figure it is
        // going to settle against rather than computing one of its own (roadmap-v3 E5).
        .route("/api/tables/{id}/check", get(check::read::<S>))
        // The same read keyed on the order, for a counter order that sits on no table (ADR-0093).
        .route("/api/orders/{id}/check", get(check::read_for_order::<S>))
        // The order: add a line to a table, fire a line to the kitchen.
        .route("/api/tables/{id}/lines", post(lines::add::<S>))
        .route("/api/lines/{id}/fire", post(lines::fire::<S>))
        // The kitchen display: bump a ticket (mark lines prepared), durable and fanned out.
        .route("/api/kds/bump", post(kds::bump::<S>))
        // The bill: open on a table, open on an order, settle. The order-keyed route is what makes
        // a takeaway order chargeable — it has no table to open a bill against (ADR-0093).
        .route("/api/tables/{id}/bill", post(bills::open::<S>))
        .route("/api/orders/{id}/bill", post(bills::open_for_order::<S>))
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
    // The counter's order list, in its own sub-router because it needs the queue-number authority
    // beside the edge and `QueueNumberAuthority` is not dyn-compatible (ADR-0093). Behind the same
    // signed-in gate as the rest of the domain surface, and the paired gate below.
    let counter = counter::router(counter_edge, queue).layer(axum::middleware::from_fn_with_state(
        Arc::clone(&sessions_for_counter),
        auth::require_signed_in,
    ));
    // Binding a terminal's print agent, in its own sub-router for the same reason and behind the
    // same two gates: `PrintAgents` is not dyn-compatible either, and ADR-0112 puts these two writes
    // behind a *manager* — the permission is checked in the handler, over the published roster.
    let agents = print_agent::router(agent_edge, agents).layer(
        axum::middleware::from_fn_with_state(sessions_for_agents, auth::require_signed_in),
    );

    guarded
        .merge(session)
        .merge(counter)
        .merge(agents)
        .layer(axum::middleware::from_fn_with_state(
            pairing,
            auth::require_paired_device,
        ))
        // Applied last, so it is *outermost* over all twenty-two domain routes and over the paired
        // gate. A preflight carries no `Authorization` by specification, so a CORS layer applied
        // inside the gate would answer every preflight `401` and every cross-origin call would fail
        // — reading to an operator as "pairing is broken" (ADR-0111).
        .layer(crate::origins::cors_layer(origins))
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
        | AppError::UnknownOrder
        | AppError::BillAlreadyOpen
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
