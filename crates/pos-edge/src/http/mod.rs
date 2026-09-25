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
mod live;
pub mod locale;
pub mod menu;
pub mod pair;
mod print_agent;
mod print_jobs;
mod printers;
pub mod qr;
pub mod reason_codes;
pub mod shifts;
mod sync;
pub mod tables;
pub mod ws;

use std::str::FromStr;
use std::sync::Arc;
use std::time::Duration;

use axum::Router;
use axum::extract::{Request, State};
use axum::http::{HeaderName, HeaderValue, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use tower_http::compression::CompressionLayer;
use tower_http::compression::predicate::{NotForContentType, Predicate as _, SizeAbove};
use tower_http::timeout::TimeoutLayer;
use tower_http::trace::TraceLayer;

use pos_core::error::DomainError;
use pos_ports::event_store::EventStore;
use pos_ports::subject_store::SubjectStore;
use pos_proto::ulid::Ulid;

use crate::app::{AppError, Edge};
use crate::auth::{Lockout, Sessions};
use crate::lease_state::CurrentStanding;
use crate::pairing::Pairing;
use crate::state::AppState;

/// The header every `/api/*` response carries: the edge's release version
/// ([ADR-0111](../../../docs/adr/0111-a-second-origin-may-address-the-edge.md)).
///
/// Named here rather than in [`crate::version`] because it is an HTTP concern; `crate::origins`
/// reads it too, to expose it across an origin.
pub(crate) const EDGE_VERSION_HEADER: HeaderName = HeaderName::from_static("pos-edge-version");

/// The header every `/api/*` response carries beside the version: whether a replacement machine
/// has taken this store
/// ([ADR-0123](../../../docs/adr/0123-a-superseded-box-opens-nothing-new.md)).
///
/// One of `active`, `superseded` or `invalid`. `crate::origins` exposes it across an origin for the
/// same reason it exposes the version: a cross-origin page cannot read a header nobody listed.
pub(crate) const LEASE_STANDING_HEADER: HeaderName = HeaderName::from_static("pos-lease-standing");

/// The header a refused or failed domain command carries beside its plain-text reason: a stable,
/// `UPPER_SNAKE_CASE` token naming *which* refusal it was
/// ([ADR-0137](../../../docs/adr/0137-a-deep-outbox-warns-and-never-refuses.md)).
///
/// The body stays the English sentence it always was, so nothing that reads it changes. The token is
/// what a till translates: an operator working in Vietnamese was shown *"the store is unavailable"*
/// or *"the command was refused: …"* verbatim, with no next step, because the only thing the screen
/// had to go on was a sentence written for a log. The vocabulary is AIP-193's `reason` — the
/// machine-readable half of an error — carried in a header because the edge's error bodies predate
/// the JSON shape and changing them would break every caller that shows them.
pub(crate) const ERROR_REASON_HEADER: HeaderName = HeaderName::from_static("pos-error-reason");

/// Stamps [`EDGE_VERSION_HEADER`] on every `/api/*` response.
///
/// # Why a response header, and why on the whole application
///
/// Version drift between an app and the edge it talks to shows up *after* pairing — an OTA ring
/// moves the edge on a Tuesday, or a shell updates itself overnight — so a value read once at
/// pairing time is a value that was true once. A response header rides the answer the app already
/// asked for, and arrives on the call that just failed rather than on a poll the app had to guess
/// the timing of.
///
/// Applied to the merged application and gated on the path, rather than per sub-router the way the
/// CORS layer is. The two are not the same kind of decision: CORS coverage is a *policy* about which
/// routes another origin may address, so a route is covered because a constructor named it. This is
/// a fact about the binary, true of every `/api` answer it gives — **including the `200 text/html`
/// the asset fallback returns for a path one side moved**, which is precisely the failure the header
/// exists to explain and which no `/api` sub-router would ever see.
async fn stamp_edge_version(
    State(standing): State<Arc<CurrentStanding>>,
    request: Request,
    next: Next,
) -> Response {
    let is_api = request.uri().path().starts_with("/api/");
    let mut response = next.run(request).await;
    if !is_api {
        return response;
    }
    if let Ok(value) = HeaderValue::from_str(crate::version::VERSION) {
        response.headers_mut().insert(EDGE_VERSION_HEADER, value);
    }
    // And the box's standing, on the same answer and for the same reason the version rides here
    // (ADR-0123): a supersession happens *during* service, so a value the app read once at pairing
    // time is a value that was true once. `from_static` on a token this enum can only produce, so
    // the fallible construction the version needs is not needed here.
    response.headers_mut().insert(
        LEASE_STANDING_HEADER,
        HeaderValue::from_static(standing.token()),
    );
    response
}

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
        //
        // The span is built by hand rather than taken from `DefaultMakeSpan`, which records the
        // whole **URI**. That comment above was aspirational until ADR-0117: the URI carries the
        // query string, `/pair?code=NNNNNN` is a route this server serves, and a durable log makes
        // the difference between a path and a URI the difference between a spent credential and a
        // live one. `uri().path()` is what the sentence always claimed.
        .layer(
            TraceLayer::new_for_http().make_span_with(|request: &axum::http::Request<_>| {
                tracing::info_span!(
                    "request",
                    method = %request.method(),
                    path = request.uri().path(),
                )
            }),
        )
        .with_state(state)
}

/// How long a request may take before the edge gives up on it.
///
/// Generous on purpose. Nothing a till asks for is slow — the domain routes are SQLite reads and
/// appends on the same machine — with one exception: verifying a PIN runs Argon2, which is
/// deliberately expensive and is the one legitimate request that can take a noticeable fraction of a
/// second on the cheap hardware a store actually buys. A budget tight enough to be interesting would
/// have to be tight enough to refuse a sign-in on a busy box, which trades a real outage for a
/// theoretical one.
///
/// So this is not a performance target. It is the line past which a request is no longer a request:
/// a connection that declared a body and never sent it, a handler that will not return. A minute
/// costs a genuine slow caller nothing and turns an indefinite hold into an answer and a log line.
///
/// # Why a minute and not thirty seconds
///
/// One route on this edge legitimately waits on the internet: `POST /api/activate` makes a cloud
/// round trip inside the request, and `server::ACTIVATION_TIMEOUT` gives that trip **thirty
/// seconds** before it fails on its own terms. A deadline set to the same figure would race it, and
/// on a slow link the race would usually be won here — turning "activation was slow and worked" into
/// `408`, on the one-shot step that brings a new store online, where a confusing failure costs an
/// engineer a site visit.
///
/// The relationship is asserted at compile time beside that constant rather than written down here
/// and remembered, because the two are a pair and the next person to tune either will be looking at
/// only one of them.
///
/// A constant rather than a setting, because no store has a reason to differ and a knob nobody turns
/// is a knob that goes wrong (`docs/design-principles.md`). If one ever does, it becomes a published
/// value then.
pub const REQUEST_TIMEOUT: Duration = Duration::from_secs(60);

/// Gives every request a deadline.
///
/// Applied by the caller to the **fully merged** application, for the reason [`stamp_version`] is:
/// [`Router::layer`] wraps only what is registered when it is called, and the forty domain routes
/// are merged afterwards. Those are the routes a till sells through, and a timeout that covered the
/// three pairing routes and the asset fallback alone would be decoration.
///
/// Measured before it existed: a connection that sent `Content-Length: 40` and then nothing held a
/// socket and a task **indefinitely** — the probe gave up after five seconds rather than the server
/// doing so. With this, the same connection is answered `408 Request Timeout` at the budget. That is
/// the whole of what it is for: a store's till has no business holding a connection open forever for
/// anyone, least of all on a shop network a guest's phone can reach ([ADR-0111](../../docs/adr/0111-a-second-origin-may-address-the-edge.md)).
///
/// **It does not touch the fan-out.** A `/ws` upgrade produces its `101` at once and the socket
/// lives on outside the request this layer is timing, so a kitchen display connected all service is
/// unaffected — asserted in `tests/request_timeout.rs` rather than reasoned about, because a layer
/// that quietly severed every display at thirty seconds would be a far worse bug than the one this
/// fixes.
pub fn time_out(app: Router) -> Router {
    time_out_for(app, REQUEST_TIMEOUT)
}

/// [`time_out`] with the budget named, for a test that cannot wait thirty seconds to watch a
/// deadline pass.
///
/// Separate so the suite exercises *this* function rather than building its own layer and proving
/// only that `tower-http` works. The shipped call is [`time_out`] and there is deliberately no way
/// to configure the budget from outside the binary — see [`REQUEST_TIMEOUT`].
pub fn time_out_for(app: Router, budget: Duration) -> Router {
    // `408`, named rather than defaulted. It is what a request that outran its own deadline is —
    // the caller took too long, not the store — and it is what a proxy or a client library knows how
    // to retry. `TimeoutLayer::new` picks the same status and is deprecated in favour of saying so.
    app.layer(TimeoutLayer::with_status_code(
        StatusCode::REQUEST_TIMEOUT,
        budget,
    ))
}

/// The smallest body worth compressing, in bytes. Below this a refusal or a one-line answer gains
/// nothing a gzip header and a deflate stream do not cost back.
pub const COMPRESS_ABOVE: u64 = 1024;

/// Gzips a response for a client that asks, when it is large enough to be worth it
/// ([ADR-0138](../../../docs/adr/0138-the-edge-compresses-what-it-sends.md)).
///
/// The menu, the floor and the live orders are re-read on every reload, wake and `resync`, and on
/// a large store they are about half a megabyte a time uncompressed, over the shop's own Wi-Fi. gzip
/// takes them down by roughly 90%.
///
/// Negotiated: a client that sends no `Accept-Encoding: gzip` — a print agent, a `curl` in a runbook
/// — gets exactly the bytes it got before. A `/ws` upgrade is a `101` with no body, so the size floor
/// never lets it through; images and event streams are excluded by content type. Applied by the
/// caller to the fully merged application, for the reason [`stamp_version`] is.
pub fn compress(app: Router) -> Router {
    app.layer(
        CompressionLayer::new().compress_when(
            SizeAbove::new(COMPRESS_ABOVE)
                .and(NotForContentType::GRPC)
                .and(NotForContentType::IMAGES)
                .and(NotForContentType::SSE),
        ),
    )
}

/// Stamps the release and the lease standing onto every `/api/*` answer this application gives.
///
/// Applied by the caller, to the **fully merged** application, and that is not a style preference.
/// [`Router::layer`] wraps only the routes registered at the moment it is called; a sub-router
/// merged in afterwards is not covered. Applied inside [`router`], as it was, it reached the three
/// pairing routes and the asset fallback and left the forty domain routes and the two activation
/// routes bare — which is every route a till actually sells through.
///
/// That made both banners the header feeds read as "nothing to say". The version-drift banner
/// ([ADR-0111](../../docs/adr/0111-a-second-origin-may-address-the-edge.md)) and the superseded
/// banner ([ADR-0123](../../docs/adr/0123-a-superseded-box-opens-nothing-new.md)) are both
/// fail-silent by design: a missing header leaves the previous value, so absence is indistinguishable
/// from agreement. A gap here is quiet rather than loud, which is why it needs saying out loud.
///
/// Outermost, so it also stamps a response a layer below refused — the call that just failed is
/// exactly the one an operator is looking at when they ask what this box is running.
pub fn stamp_version(app: Router, standing: Arc<CurrentStanding>) -> Router {
    app.layer(axum::middleware::from_fn_with_state(
        standing,
        stamp_edge_version,
    ))
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
pub fn domain_router<S, Q, A, J, W>(
    edge: Arc<Edge<S>>,
    queue: Q,
    agents: A,
    jobs: J,
    wake: W,
    pairing: Arc<Pairing>,
    sessions: Arc<Sessions>,
    origins: &Arc<crate::origins::Origins>,
) -> Router
where
    S: EventStore + SubjectStore + Send + Sync + 'static,
    Q: crate::queue::QueueNumberAuthority + 'static,
    A: crate::print_agent::PrintAgents + Clone + 'static,
    J: crate::print_queue::PrintQueue + 'static,
    W: crate::print_wake::PrintWake + 'static,
{
    let lockout = Arc::new(Lockout::new());
    // Cloned before `edge` and `sessions` move into the routers below.
    let counter_edge = Arc::clone(&edge);
    let agent_edge = Arc::clone(&edge);
    let jobs_edge = Arc::clone(&edge);
    let codes_edge = Arc::clone(&edge);
    let sessions_for_counter = Arc::clone(&sessions);
    let sessions_for_agents = Arc::clone(&sessions);
    let sessions_for_codes = Arc::clone(&sessions);

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
        // Staff mark an item sold out, and bring it back (86). The events have been in the schema
        // since it was written; this is the first thing to emit them.
        .route("/api/menu/{id}/sold-out", post(menu::sold_out::<S>))
        .route("/api/menu/{id}/restore", post(menu::restore::<S>))
        // How the till groups and orders those items, from the `layout` node the same publish writes
        // (ADR-0066, production-readiness C4). A separate node, so a separate route: a price change
        // relays no buttons and a button moving reprices nothing.
        .route("/api/layout", get(layout::plan::<S>))
        // The money facts the pay pad needs — currency, the notes a guest carries, what the total
        // rounds to in cash. A country's coinage, published rather than compiled in (ADR-0105).
        .route("/api/locale", get(locale::settings::<S>))
        // The store's managed reason list, so a picker offers what the store actually holds
        // (ADR-0115). One read for every picker: each entry carries the actions it covers, and the
        // till filters by the act in hand.
        .route("/api/reason-codes", get(reason_codes::list::<S>))
        // The floor: seat, clean, read.
        .route("/api/tables/{id}/seat", post(tables::seat::<S>))
        .route("/api/tables/{id}/clean", post(tables::clean::<S>))
        .route("/api/tables/{id}", get(tables::get::<S>))
        // What the table owes right now, assembled by the edge — the till displays the figure it is
        // going to settle against rather than computing one of its own (roadmap-v3 E5).
        .route("/api/tables/{id}/check", get(check::read::<S>))
        // The same read keyed on the order, for a counter order that sits on no table (ADR-0093).
        .route("/api/orders/{id}/check", get(check::read_for_order::<S>))
        // What is open right now, with the line ids to act on it. A device learns lines from the
        // fan-out, which carries what happens next — so without this read a till that reloads and a
        // kitchen display switched on mid-service both draw an empty screen over live food.
        .route("/api/orders/live", get(live::read::<S>))
        // The order: add a line to a table, fire a line to the kitchen.
        .route("/api/tables/{id}/lines", post(lines::add::<S>))
        .route("/api/lines/{id}/fire", post(lines::fire::<S>))
        // How many, while the line is still editable. The till could only ever add one of a thing,
        // so "three beers" was three lines.
        .route("/api/lines/{id}/quantity", post(lines::set_quantity::<S>))
        // And the whole order in one tap. Firing line by line cost an operator one tap per line and
        // left half an order with the kitchen when one of them failed; this commits them together.
        .route("/api/orders/{id}/fire", post(lines::fire_order::<S>))
        // And one course of it, which is what `courses_enabled` gates (ADR-0130). The narrowing of
        // the act above, not a different one: same transaction boundary, same routing, same gate.
        .route(
            "/api/orders/{id}/fire/{course_id}",
            post(lines::fire_course::<S>),
        )
        // The staff-confirmation queue (ADR-0116). A guest's tabled QR order cannot be fired until
        // one of these two decisions lands, which is the guardrail ADR-0012 promised.
        .route("/api/orders/awaiting-confirmation", get(qr::awaiting::<S>))
        .route("/api/orders/{id}/confirm", post(qr::confirm::<S>))
        .route("/api/orders/{id}/reject", post(qr::reject::<S>))
        // The kitchen display: bump a ticket (mark lines prepared), durable and fanned out.
        .route("/api/kds/bump", post(kds::bump::<S>))
        // The bill: open on a table, open on an order, settle. The order-keyed route is what makes
        // a takeaway order chargeable — it has no table to open a bill against (ADR-0093).
        .route("/api/tables/{id}/bill", post(bills::open::<S>))
        .route("/api/orders/{id}/bill", post(bills::open_for_order::<S>))
        .route("/api/bills/{id}/settle", post(bills::settle::<S>))
        // What one bill owes and the lines it covers — the read a split needs, because once a
        // table's bill is split each guest at the till is asking about their own part (ADR-0128).
        .route("/api/bills/{id}/check", get(check::read_for_bill::<S>))
        // The void pair (ADR-0115, roadmap B2.2). A fired line and any bill need a manager's
        // PIN with the request, which is the first thing in the tree to read the permission
        // registry's `pin: true`.
        .route("/api/lines/{id}/void", post(lines::void::<S>))
        .route("/api/bills/{id}/void", post(bills::void::<S>))
        .route("/api/bills/{id}/discount", post(bills::discount::<S>))
        // Split and merge (ADR-0128). Neither carries a permission or a PIN: both move amounts
        // that are already captured, and a prompt here would be paid for on every table.
        .route("/api/bills/{id}/split", post(bills::split::<S>))
        .route("/api/bills/{id}/merge", post(bills::merge::<S>))
        // The cash shift: open, blind count, close — and the one that is open, so a device that
        // reloads mid-shift can still count and close it rather than offering to open another.
        .route("/api/shifts/current", get(shifts::current::<S>))
        .route("/api/shifts", post(shifts::open::<S>))
        .route("/api/shifts/{id}/count", post(shifts::count::<S>))
        .route("/api/shifts/{id}/close", post(shifts::close::<S>))
        // The cloud link and the outbox, for the status bar (ADR-0137): "Offline — selling
        // normally" and how many events are waiting.
        .route("/api/sync", get(sync::read::<S>))
        // The published printers, and a manager's test page on one — how a new store learns a
        // printer is wired before a guest's receipt tells it.
        .route("/api/printers", get(printers::list::<S>))
        .route("/api/printers/{id}/test", post(printers::test::<S>))
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
    let binding = print_agent::router(agent_edge, agents.clone()).layer(
        axum::middleware::from_fn_with_state(sessions_for_agents, auth::require_signed_in),
    );
    // Minting the pairing code for the next device (ADR-0118): its own sub-router for the same
    // reason — it reads the published roster off the application `Edge`, which is generic over the
    // store — and behind the same two gates. A paired device *and* a signed-in manager, because
    // issuing a credential is a stronger act than the paired-only posture `/api/pair/revoke` has.
    let codes = pair::codes_router(codes_edge, Arc::clone(&pairing)).layer(
        axum::middleware::from_fn_with_state(sessions_for_codes, auth::require_signed_in),
    );
    // And the agent's own two routes, which carry the paired gate **and no second one**: an agent is
    // an unattended process, so requiring a sign-in would mean a manager's PIN before every kitchen
    // ticket (ADR-0112). They join the five paired-only routes the edge already serves rather than
    // inventing a sixth kind of gate. The binding is what says which terminal the caller answers
    // for; nothing in the request gets to claim it.
    let jobs = print_jobs::router(jobs_edge, agents, jobs, wake);

    guarded
        .merge(session)
        .merge(counter)
        .merge(binding)
        .merge(codes)
        .merge(jobs)
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
    refusal(StatusCode::BAD_REQUEST, "INVALID_ARGUMENT", what.to_owned())
}

/// A refusal: the status, the stable [`ERROR_REASON_HEADER`] token, and the English sentence.
fn refusal(status: StatusCode, reason: &'static str, message: String) -> Response {
    (
        status,
        [(ERROR_REASON_HEADER, HeaderValue::from_static(reason))],
        message,
    )
        .into_response()
}

/// The stable token for a failure kind — what a device translates, where the body is what a log
/// reads. One per [`AppError`] variant, and one per [`DomainError`] rule, so a till can tell "the
/// bill is underpaid" from "you may not do that" without parsing English.
///
/// A token, once shipped, keeps its meaning: a till built against it translates it, and renaming it
/// would silently turn a translated message back into an English one. Add; never rename.
pub(crate) fn error_reason(error: &AppError) -> &'static str {
    match error {
        AppError::Domain(inner) => match inner {
            DomainError::Money(_) => "MONEY_INVALID",
            DomainError::Transition(_) => "TRANSITION_REFUSED",
            DomainError::PaymentsDoNotSumToTotal { .. } => "PAYMENTS_DO_NOT_SUM_TO_TOTAL",
            DomainError::NegativeChange => "NEGATIVE_CHANGE",
            DomainError::ReductionExceedsBill { .. } => "REDUCTION_EXCEEDS_BILL",
            DomainError::NotAPartition { .. } => "SPLIT_NOT_A_PARTITION",
            DomainError::TaxRateNotConfigured { .. } => "TAX_RATE_NOT_CONFIGURED",
            DomainError::Empty { .. } => "EMPTY",
            DomainError::PermissionDenied { .. } => "PERMISSION_DENIED",
            DomainError::CapabilityDisabled { .. } => "CAPABILITY_DISABLED",
            // `DomainError` is non-exhaustive: a rule added later reads as a refusal until it is
            // given a token of its own.
            _ => "COMMAND_REFUSED",
        },
        AppError::NoOpenOrder => "NO_OPEN_ORDER",
        AppError::UnknownLine => "UNKNOWN_LINE",
        AppError::UnroutableLine => "UNROUTABLE_LINE",
        AppError::UnknownOrder => "UNKNOWN_ORDER",
        AppError::BillAlreadyOpen => "BILL_ALREADY_OPEN",
        AppError::UnknownBill => "UNKNOWN_BILL",
        AppError::BillsOnDifferentTables => "BILLS_ON_DIFFERENT_TABLES",
        AppError::UnknownShift => "UNKNOWN_SHIFT",
        AppError::ShiftAlreadyOpen => "SHIFT_ALREADY_OPEN",
        AppError::AwaitingStaffConfirmation => "AWAITING_STAFF_CONFIRMATION",
        AppError::OrderRejected => "ORDER_REJECTED",
        AppError::NotAwaitingStaffConfirmation => "NOT_AWAITING_STAFF_CONFIRMATION",
        AppError::ReasonCodeNotValid => "REASON_CODE_NOT_VALID",
        AppError::VoidReasonNotValid => "VOID_REASON_NOT_VALID",
        AppError::ModifierSelectionInvalid => "MODIFIER_SELECTION_INVALID",
        AppError::ChannelNotAccepted => "CHANNEL_NOT_ACCEPTED",
        AppError::ItemNotSellable => "ITEM_NOT_SELLABLE",
        AppError::AlreadyFired => "ALREADY_FIRED",
        AppError::ApprovalRequired => "APPROVAL_REQUIRED",
        AppError::ApprovalRefused => "APPROVAL_REFUSED",
        AppError::Superseded => "SUPERSEDED",
        AppError::Port(_) => "STORE_UNAVAILABLE",
        AppError::Clock | AppError::Encode(_) => "INTERNAL",
    }
}

/// Maps a refused or failed command to a status — the one place the edge decides which HTTP code a
/// failure kind is, so every domain route answers the same way.
///
/// A refused command (an illegal transition, a missing permission, a disabled capability, or a
/// table/line/bill/shift that is not in a state the command applies to) is the caller's fault, so it
/// is `409 Conflict` rather than `500`. An unreachable store is `503`; a clock or encoding failure is
/// the edge's own `500`.
pub(crate) fn error_response(error: &AppError) -> Response {
    let reason = error_reason(error);
    match error {
        AppError::Domain(inner) => refusal(StatusCode::CONFLICT, reason, inner.to_string()),
        AppError::NoOpenOrder
        | AppError::UnknownLine
        | AppError::UnroutableLine
        | AppError::UnknownOrder
        | AppError::BillAlreadyOpen
        | AppError::UnknownBill
        | AppError::BillsOnDifferentTables
        | AppError::UnknownShift
        | AppError::ShiftAlreadyOpen
        | AppError::AwaitingStaffConfirmation
        | AppError::OrderRejected
        | AppError::NotAwaitingStaffConfirmation
        | AppError::ReasonCodeNotValid
        | AppError::VoidReasonNotValid
        | AppError::ModifierSelectionInvalid
        | AppError::ChannelNotAccepted
        | AppError::ItemNotSellable
        | AppError::AlreadyFired => refusal(StatusCode::CONFLICT, reason, error.to_string()),
        // A missing or refused manager PIN is an authorisation failure, not a state conflict: the
        // command is well-formed and applies to the record, and the only thing missing is the
        // authority to run it. `403` and not `401`, because the *caller* is authenticated — it is
        // the act that needs a second person (`docs/pos-spec.md` §9, §11.4).
        //
        // A superseded box joins them on the same reading (ADR-0123): the command is well-formed
        // and the record is fine, and what is missing is the authority to run it — here the
        // *machine's*, not the actor's, because a replacement holds this store's lease. Not `409`,
        // which would say the caller asked at the wrong moment; not `503`, which would promise that
        // trying again helps. It does not: the way back is to re-provision this box.
        AppError::ApprovalRequired | AppError::ApprovalRefused | AppError::Superseded => {
            refusal(StatusCode::FORBIDDEN, reason, error.to_string())
        }
        AppError::Port(_) => refusal(
            StatusCode::SERVICE_UNAVAILABLE,
            reason,
            "the store is unavailable".to_owned(),
        ),
        AppError::Clock | AppError::Encode(_) => refusal(
            StatusCode::INTERNAL_SERVER_ERROR,
            reason,
            "the edge could not apply the command".to_owned(),
        ),
    }
}
