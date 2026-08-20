// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The cloud HTTP surface.
//!
//! Three kinds of route:
//!
//!  * `/health` — liveness.
//!  * `/internal/*` — the ingest re-push target the reconciliation loop uses (`docs/roadmap.md`
//!    P7); the primary production path is the NATS cursor feed. Not part of the external contract.
//!  * `/v1/*` — the **public** API external integrators build against. Every `/v1` handler carries a
//!    [`utoipa::path`] annotation and every response type derives `utoipa::ToSchema`, so
//!    `/v1/openapi.json` is generated from the code and can never drift from it
//!    ([ADR-0019](../../../docs/adr/0019-openapi-generation.md)).
//!
//! The router is generic over the store `S`, so tests drive it against `pos-fakes` and the binary
//! serves it over `store-postgres` — the identical handler code (ADR-0026).

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};

use pos_ports::PortError;
use pos_ports::event_store::EventStore;
use pos_proto::ErrorStatus;
use pos_proto::envelope::{EventEnvelope, RawPayload};
use pos_proto::ids::StoreId;
use pos_proto::ulid::Ulid;

use crate::cloud::{Cloud, DailyRollup};
use crate::openapi::ApiDoc;
use utoipa::OpenApi as _;

/// Builds the cloud router over `cloud`.
pub fn router<S>(cloud: Cloud<S>) -> Router
where
    S: EventStore + Clone + Send + Sync + 'static,
    S::Tx: Send,
{
    Router::new()
        .route("/health", get(health))
        .route("/internal/ingest", post(ingest::<S>))
        .route(
            "/v1/stores/{store_id}/rollups/daily",
            get(daily_rollups::<S>),
        )
        .route("/v1/openapi.json", get(openapi))
        .with_state(cloud)
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
/// so it is deliberately absent from the public OpenAPI document.
async fn ingest<S>(
    State(cloud): State<Cloud<S>>,
    Json(events): Json<Vec<EventEnvelope<RawPayload>>>,
) -> Response
where
    S: EventStore + Clone + Send + Sync + 'static,
    S::Tx: Send,
{
    match cloud.ingest(&events).await {
        Ok(outcome) => (StatusCode::OK, Json(outcome)).into_response(),
        Err(error) => error_response(&error),
    }
}

/// A store's per-trading-day activity rollups, oldest day first.
#[utoipa::path(
    get,
    path = "/v1/stores/{store_id}/rollups/daily",
    params(("store_id" = String, Path, description = "The store's 26-character ULID")),
    responses(
        (status = 200, description = "Daily activity rollups, oldest day first", body = Vec<DailyRollup>),
        (status = 400, description = "The store id is not a ULID"),
        (status = 503, description = "The store is unreachable"),
    ),
    tag = "rollups",
)]
pub(crate) async fn daily_rollups<S>(
    State(cloud): State<Cloud<S>>,
    Path(store_id): Path<String>,
) -> Response
where
    S: EventStore + Clone + Send + Sync + 'static,
    S::Tx: Send,
{
    let store_id = match store_id.parse::<Ulid>() {
        Ok(ulid) => StoreId::new(ulid),
        Err(_) => {
            return (StatusCode::BAD_REQUEST, "the store id is not a ULID").into_response();
        }
    };
    match cloud.daily_rollups(store_id).await {
        Ok(rollups) => (StatusCode::OK, Json(rollups)).into_response(),
        Err(error) => error_response(&error),
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
