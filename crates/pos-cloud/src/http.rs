// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The cloud HTTP surface.
//!
//! Three kinds of route:
//!
//!  * `/health` — liveness.
//!  * `/internal/*` — the ingest re-push target the reconciliation loop uses (`docs/roadmap.md`
//!    P7); the primary production path is the NATS cursor feed. Not part of the external contract,
//!    and not authenticated — it is reachable only on the cloud's own private network.
//!  * `/v1/*` — the **public** API external integrators build against. Every data route requires a
//!    scoped per-tenant API key ([`crate::auth::bearer`]) and answers only for the key's own tenant.
//!    Every `/v1` handler carries a [`utoipa::path`] annotation and every response type derives
//!    `utoipa::ToSchema`, so `/v1/openapi.json` is generated from the code and can never drift from
//!    it ([ADR-0019](../../../docs/adr/0019-openapi-generation.md)).
//!
//! The router is generic over its collaborators — the [`EventStore`], the [`RollupStore`], the
//! [`ApiKeyStore`], and the [`ClockSource`] — bundled in [`CloudApp`]. Tests drive it against
//! `pos-fakes` and the binary serves it over `store-postgres` with the identical handler code
//! (ADR-0026).

use core::fmt;

use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};

use pos_ports::PortError;
use pos_ports::event_store::EventStore;
use pos_proto::ErrorStatus;
use pos_proto::determinism::ClockSource;
use pos_proto::envelope::{EventEnvelope, RawPayload};
use pos_proto::ids::StoreId;
use pos_proto::ulid::Ulid;

use crate::auth::apikey::{ApiKeyStore, Scope};
use crate::auth::bearer::{authenticate, require_scope};
use crate::cloud::{Cloud, DailyRollup};
use crate::dashboard::{RollupError, RollupStore, dashboard};
use crate::openapi::ApiDoc;
use utoipa::OpenApi as _;

/// Everything a request handler needs, bundled so the router carries one state type: the event
/// store's application layer, the materialised rollup read model, the API-key store the `/v1`
/// bearer check consults, and the clock that check verifies expiry against.
///
/// Cloneable and cheap to clone — each collaborator is itself a shared handle (a pool, an `Arc`), so
/// a clone talks to the same backing store.
pub struct CloudApp<S, R, K, C> {
    cloud: Cloud<S>,
    rollups: R,
    keys: K,
    clock: C,
}

impl<S, R, K, C> fmt::Debug for CloudApp<S, R, K, C> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        // The collaborators are opaque handles — a pool, a key store, a clock — and some hold
        // secrets, so the fields are deliberately elided rather than rendered.
        formatter.debug_struct("CloudApp").finish_non_exhaustive()
    }
}

impl<S, R, K, C> Clone for CloudApp<S, R, K, C>
where
    S: Clone,
    R: Clone,
    K: Clone,
    C: Clone,
{
    fn clone(&self) -> Self {
        Self {
            cloud: self.cloud.clone(),
            rollups: self.rollups.clone(),
            keys: self.keys.clone(),
            clock: self.clock.clone(),
        }
    }
}

impl<S, R, K, C> CloudApp<S, R, K, C> {
    /// Bundles the collaborators into one shareable application state.
    pub const fn new(cloud: Cloud<S>, rollups: R, keys: K, clock: C) -> Self {
        Self {
            cloud,
            rollups,
            keys,
            clock,
        }
    }
}

/// Builds the cloud router over `app`.
pub fn router<S, R, K, C>(app: CloudApp<S, R, K, C>) -> Router
where
    S: EventStore + Clone + Send + Sync + 'static,
    S::Tx: Send,
    R: RollupStore + Clone + Send + Sync + 'static,
    K: ApiKeyStore + Clone + Send + Sync + 'static,
    C: ClockSource + Clone + Send + Sync + 'static,
{
    Router::new()
        .route("/health", get(health))
        .route("/internal/ingest", post(ingest::<S, R, K, C>))
        .route(
            "/v1/stores/{store_id}/rollups/daily",
            get(daily_rollups::<S, R, K, C>),
        )
        .route("/v1/openapi.json", get(openapi))
        .with_state(app)
}

/// Liveness: answers as soon as the process is serving.
async fn health() -> &'static str {
    "ok"
}

/// The generated OpenAPI document for the public `/v1` surface.
async fn openapi() -> Json<utoipa::openapi::OpenApi> {
    Json(ApiDoc::openapi())
}

/// Ingests a batch of event envelopes, idempotently. Internal (the reconciliation re-push target),
/// so it is deliberately absent from the public OpenAPI document and carries no authentication.
async fn ingest<S, R, K, C>(
    State(app): State<CloudApp<S, R, K, C>>,
    Json(events): Json<Vec<EventEnvelope<RawPayload>>>,
) -> Response
where
    S: EventStore + Clone + Send + Sync + 'static,
    S::Tx: Send,
    // R, K, C are unused here, but the shared `CloudApp` state must be `Clone + Send + Sync` for the
    // `State` extractor, and that decomposes to every field being so.
    R: Clone + Send + Sync + 'static,
    K: Clone + Send + Sync + 'static,
    C: Clone + Send + Sync + 'static,
{
    match app.cloud.ingest(&events).await {
        Ok(outcome) => (StatusCode::OK, Json(outcome)).into_response(),
        Err(error) => error_response(&error),
    }
}

/// A store's per-trading-day activity rollups, oldest day first — answered from the materialised
/// rollup, never a log scan.
///
/// Requires a valid API key with the `read_rollups` scope, and answers **only** for that key's
/// tenant: the tenant comes from the verified grant, never the request, so a caller can read a
/// store's rollups only if the store is within its own tenant. A `store_id` outside the tenant is
/// not an error — it simply has no rollup and reads back as an empty list.
#[utoipa::path(
    get,
    path = "/v1/stores/{store_id}/rollups/daily",
    params(("store_id" = String, Path, description = "The store's 26-character ULID")),
    security(("api_key" = [])),
    responses(
        (status = 200, description = "Daily activity rollups, oldest day first", body = Vec<DailyRollup>),
        (status = 400, description = "The store id is not a ULID"),
        (status = 401, description = "The API key is missing, malformed, or invalid"),
        (status = 403, description = "The API key lacks the read_rollups scope"),
        (status = 503, description = "The rollup store is unreachable"),
    ),
    tag = "rollups",
)]
pub(crate) async fn daily_rollups<S, R, K, C>(
    State(app): State<CloudApp<S, R, K, C>>,
    headers: HeaderMap,
    Path(store_id): Path<String>,
) -> Response
where
    // S is unused here but is part of the shared `CloudApp` state, which the `State` extractor needs
    // whole as `Clone + Send + Sync`.
    S: Clone + Send + Sync + 'static,
    R: RollupStore + Clone + Send + Sync + 'static,
    K: ApiKeyStore + Clone + Send + Sync + 'static,
    C: ClockSource + Clone + Send + Sync + 'static,
{
    // Identity first: who is calling, and are they who they claim? Then authorisation: may this key
    // read rollups at all? Only then does the request touch a resource.
    let grant = match authenticate(&app.keys, &app.clock, &headers).await {
        Ok(grant) => grant,
        Err(denied) => return denied.into_response(),
    };
    if let Err(forbidden) = require_scope(&grant, Scope::ReadRollups) {
        return forbidden.into_response();
    }
    let store_id = match store_id.parse::<Ulid>() {
        Ok(ulid) => StoreId::new(ulid),
        Err(_) => {
            return (StatusCode::BAD_REQUEST, "the store id is not a ULID").into_response();
        }
    };
    // The tenant is the grant's, not the request's — this is the isolation boundary.
    match dashboard(&app.rollups, grant.tenant(), store_id).await {
        Ok(rollups) => (StatusCode::OK, Json(rollups)).into_response(),
        Err(error) => rollup_error_response(&error),
    }
}

/// Maps a [`PortError`] to an HTTP response, translating the AIP-193 status to a status code so a
/// caller retries the retryable ones (`503`, `429`) and not the terminal ones.
fn error_response(error: &PortError) -> Response {
    let status = match error.status() {
        ErrorStatus::InvalidArgument => StatusCode::BAD_REQUEST,
        ErrorStatus::FailedPrecondition => StatusCode::CONFLICT,
        ErrorStatus::ResourceExhausted => StatusCode::TOO_MANY_REQUESTS,
        ErrorStatus::Unavailable => StatusCode::SERVICE_UNAVAILABLE,
        _ => StatusCode::INTERNAL_SERVER_ERROR,
    };
    (status, error.to_string()).into_response()
}

/// Maps a rollup-read failure to a `503`, logging the detail rather than returning it — a dashboard
/// read only fails when the store itself is unreachable, which is transient and the caller's cue to
/// retry, and the internal reason is not the client's business.
fn rollup_error_response(error: &RollupError) -> Response {
    tracing::error!(%error, "a dashboard rollup read failed");
    (
        StatusCode::SERVICE_UNAVAILABLE,
        "the dashboard is temporarily unavailable",
    )
        .into_response()
}
