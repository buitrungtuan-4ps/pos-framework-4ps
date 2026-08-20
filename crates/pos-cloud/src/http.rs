// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The cloud HTTP surface.
//!
//! This slice exposes the ingest path and a health check. Ingest is `/internal/*`, not public
//! `/v1`: in production the primary path is the NATS cursor feed, and this endpoint is the
//! re-push target the reconciliation loop uses (`docs/roadmap.md` P7). The public, OpenAPI-described
//! `/v1` read API ([ADR-0019](../../../docs/adr/0019-openapi-generation.md)) lands with the
//! dashboard slice, so nothing here claims to be `/v1` yet.
//!
//! The router is generic over the store `S`, so tests drive it against `pos-fakes` and the binary
//! serves it over `store-postgres` — the identical handler code (ADR-0026).

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};

use pos_ports::PortError;
use pos_ports::event_store::EventStore;
use pos_proto::ErrorStatus;
use pos_proto::envelope::{EventEnvelope, RawPayload};

use crate::cloud::Cloud;

/// Builds the cloud router over `cloud`.
pub fn router<S>(cloud: Cloud<S>) -> Router
where
    S: EventStore + Clone + Send + Sync + 'static,
    S::Tx: Send,
{
    Router::new()
        .route("/health", get(health))
        .route("/internal/ingest", post(ingest::<S>))
        .with_state(cloud)
}

/// Liveness: answers as soon as the process is serving.
async fn health() -> &'static str {
    "ok"
}

/// Ingests a batch of event envelopes, idempotently.
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
