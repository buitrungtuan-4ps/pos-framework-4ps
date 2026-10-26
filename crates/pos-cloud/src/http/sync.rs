// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The store-facing configuration surface (`/sync`): config sync, the heartbeat, and the events a
//! store publishes.

use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use pos_cloud_api::response::{api_error, api_error_with_details, parse_ulid_fields};
use pos_ports::config_store::ConfigUpdate;
use pos_ports::event_store::EventStore;
use pos_proto::ErrorStatus;
use pos_proto::determinism::ClockSource;
use pos_proto::envelope::{EventEnvelope, RawPayload};
use pos_proto::ids::{ConfigVersionId, StoreId};
use serde::{Deserialize, Serialize};

use crate::auth::apikey::{ApiKeyStore, Scope};
use crate::auth::bearer::{authenticate, require_scope, require_store};
use crate::config_tree::{CapabilityValidator, ConfigTree, ConfigTreeStore, SyncOutcome};
use crate::fleet::PrintAgentStanding;
use crate::http::config_publish::strip_tree_version;
use crate::http::{
    CloudApp, Json, config_store_error_response, error_response, no_published_configuration,
};

// --- The store-facing configuration surface (`/sync`) -------------------------------------------

/// The version a store reports it currently holds, if any.
#[derive(Debug, Clone, Deserialize)]
pub(in crate::http) struct ConfigSyncQuery {
    /// The config version the store holds (a ULID), or absent if it holds none yet.
    #[serde(default)]
    held_version: Option<String>,
}

/// What a store should do to reach the current configuration — the wire form of a
/// [`SyncOutcome`](crate::config_tree::SyncOutcome).
#[derive(Debug, serde::Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
enum ConfigSyncResponse {
    /// The store already holds the current version; it applies nothing.
    UpToDate,
    /// The store should apply this snapshot or delta.
    Update {
        /// The snapshot or collapsed delta to apply ([ADR-0033](../../../docs/adr/0033-config-tree.md)).
        update: ConfigUpdate,
    },
}

#[utoipa::path(
    get,
    path = "/sync/stores/{store_id}/config",
    security(("device_credential" = ["read_config"])),
    params(
        ("store_id" = String, Path, description = "The store, a ULID. It must be the credential's own"),
        ("held_version" = Option<String>, Query, description = "The config version the store holds, a ULID. \
                                                                Absent when it holds none"),
    ),
    responses(
        (status = 200, description = "`status`: `up_to_date`, or `update` with the snapshot or collapsed \
                                      delta that brings the store to the current version (ADR-0039)"),
        (status = 400, description = "store_id is not a ULID, or held_version is not a ULID", body = pos_cloud_api::error_schema::ErrorResponse),
        (status = 401, description = "No credential, or one the cloud does not accept: one answer for every \
                                      credential problem", body = pos_cloud_api::error_schema::ErrorResponse),
        (status = 403, description = "The credential lacks the `read_config` scope, or belongs to another \
                                      store", body = pos_cloud_api::error_schema::ErrorResponse),
        (status = 404, description = "Nothing is published for this store in the credential's tenant", body = pos_cloud_api::error_schema::ErrorResponse),
        (status = 429, description = "Too many requests to the store sync surface: `Retry-After` says when \
                                      to try again", body = pos_cloud_api::error_schema::ErrorResponse),
        (status = 503, description = "The credential store, or the configuration service, is unavailable", body = pos_cloud_api::error_schema::ErrorResponse),
    ),
    tag = "configuration",
)]
/// Serves a store its own configuration update: nothing (up to date), a delta, or a full snapshot
/// past *K* versions behind ([ADR-0039](../../../docs/adr/0039-config-delivery.md)).
///
/// Requires a valid API key with the `read_config` scope, and answers **only** for that key's tenant:
/// the tenant comes from the verified grant, never the path, so a store can pull a `store_id` only
/// within its own tenant. A store outside the tenant, or one with nothing published, reads `404`.
pub(in crate::http) async fn edge_config_sync<S, R, K, C, A, T, W>(
    State(app): State<CloudApp<S, R, K, C, A, T, W>>,
    headers: HeaderMap,
    Path(store_id): Path<String>,
    Query(query): Query<ConfigSyncQuery>,
) -> Response
where
    S: Clone + Send + Sync + 'static,
    R: Clone + Send + Sync + 'static,
    K: ApiKeyStore + Clone + Send + Sync + 'static,
    C: ClockSource + Clone + Send + Sync + 'static,
    A: Clone + Send + Sync + 'static,
    T: ConfigTreeStore + Clone + Send + Sync + 'static,
    W: Clone + Send + Sync + 'static,
{
    let grant = match authenticate(&app.keys, &app.clock, &headers).await {
        Ok(grant) => grant,
        Err(denied) => return denied.into_response(),
    };
    if let Err(forbidden) = require_scope(&grant, Scope::ReadConfig) {
        return forbidden.into_response();
    }
    let store_id = match parse_ulid_fields([("store_id", &store_id)]) {
        Ok([store_id]) => StoreId::new(store_id),
        Err(refusal) => return refusal,
    };
    if let Err(forbidden) = require_store(&grant, store_id) {
        return forbidden.into_response();
    }
    let held = match query.held_version {
        None => None,
        Some(ref raw) => match parse_ulid_fields([("held_version", raw)]) {
            Ok([version]) => Some(ConfigVersionId::new(version)),
            Err(refusal) => return refusal,
        },
    };
    // Record the store's contact and the version it reported holding, for the fleet view
    // ([ADR-0068](../../../docs/adr/0068-fleet-liveness.md)). Best-effort telemetry: a liveness-write
    // failure is logged and swallowed, never failing the config pull the store actually needs.
    if let Err(error) = app
        .config_trees
        .record_store_seen(grant.tenant(), store_id, held, app.clock.now())
        .await
    {
        tracing::warn!(%error, "recording store liveness on a config pull failed");
    }
    // The tenant is the grant's, not the path's — a store reaches only its own tenant's trees.
    match app
        .config_trees
        .load(grant.tenant(), store_id)
        .await
        .map(strip_tree_version)
    {
        Ok(Some(state)) => {
            let tree = ConfigTree::from_state(store_id, CapabilityValidator, state);
            let response = match tree.update_for(held) {
                SyncOutcome::UpToDate => ConfigSyncResponse::UpToDate,
                SyncOutcome::Deliver(update) => ConfigSyncResponse::Update { update },
            };
            (StatusCode::OK, Json(response)).into_response()
        }
        Ok(None) => no_published_configuration(),
        Err(error) => config_store_error_response(&error),
    }
}

#[utoipa::path(
    post,
    path = "/sync/stores/{store_id}/heartbeat",
    security(("device_credential" = ["read_config"])),
    params(
        ("store_id" = String, Path, description = "The store, a ULID. It must be the credential's own"),
    ),
    request_body(
        content = HeartbeatBody, description = "Optional: `outbox_depth`, `lease_generation` and `print_agents`, each left out when \
                       the store cannot say. An empty body is a bare ping",
        content_type = "application/json",
    ),
    responses(
        (status = 204, description = "Recorded: the store's last contact, and what it reported"),
        (status = 400, description = "store_id is not a ULID, or the body is not a heartbeat report", body = pos_cloud_api::error_schema::ErrorResponse),
        (status = 401, description = "No credential, or one the cloud does not accept: one answer for every \
                                      credential problem", body = pos_cloud_api::error_schema::ErrorResponse),
        (status = 403, description = "The credential lacks the `read_config` scope, or belongs to another \
                                      store", body = pos_cloud_api::error_schema::ErrorResponse),
        (status = 429, description = "Too many requests to the store sync surface: `Retry-After` says when \
                                      to try again", body = pos_cloud_api::error_schema::ErrorResponse),
        (status = 503, description = "The credential store, or the configuration service, is unavailable", body = pos_cloud_api::error_schema::ErrorResponse),
    ),
    tag = "configuration",
)]
/// `POST /sync/stores/{store_id}/heartbeat` — a store's lightweight liveness ping
/// ([ADR-0068](../../../docs/adr/0068-fleet-liveness.md) slice 2). Authenticated with the same scoped
/// API key the config pull uses (`read_config`), it advances the store's `last_seen_at` and nothing
/// else, so a store that is up but not currently pulling config (a parked long-poll, a quiet period
/// between publishes) still registers as online. `204` on success; unlike the config-pull capture,
/// recording is this request's whole purpose, so a store-write failure is a `503` the edge retries,
/// not a swallowed best-effort write.
pub(in crate::http) async fn edge_heartbeat<S, R, K, C, A, T, W>(
    State(app): State<CloudApp<S, R, K, C, A, T, W>>,
    headers: HeaderMap,
    Path(store_id): Path<String>,
    body: axum::body::Bytes,
) -> Response
where
    S: Clone + Send + Sync + 'static,
    R: Clone + Send + Sync + 'static,
    K: ApiKeyStore + Clone + Send + Sync + 'static,
    C: ClockSource + Clone + Send + Sync + 'static,
    A: Clone + Send + Sync + 'static,
    T: ConfigTreeStore + Clone + Send + Sync + 'static,
    W: Clone + Send + Sync + 'static,
{
    let grant = match authenticate(&app.keys, &app.clock, &headers).await {
        Ok(grant) => grant,
        Err(denied) => return denied.into_response(),
    };
    if let Err(forbidden) = require_scope(&grant, Scope::ReadConfig) {
        return forbidden.into_response();
    }
    let store_id = match parse_ulid_fields([("store_id", &store_id)]) {
        Ok([store_id]) => StoreId::new(store_id),
        Err(refusal) => return refusal,
    };
    if let Err(forbidden) = require_store(&grant, store_id) {
        return forbidden.into_response();
    }
    let report = match heartbeat_body(&body) {
        Ok(report) => report,
        // A body that is present and will not parse is the caller's mistake; swallowing it would let
        // a store report nothing forever while believing it reported.
        Err(error) => {
            return api_error_with_details(
                ErrorStatus::InvalidArgument,
                format!("the heartbeat body is not a heartbeat report: {error}"),
                &[
                    ("outbox_depth", "INVALID_VALUE"),
                    ("lease_generation", "INVALID_VALUE"),
                    ("print_agents", "INVALID_VALUE"),
                ],
            );
        }
    };
    // The tenant is the grant's, not the path's — a store reaches only its own tenant's liveness row.
    match app
        .config_trees
        .record_store_heartbeat(
            grant.tenant(),
            store_id,
            app.clock.now(),
            report.outbox_depth,
            report.lease_generation,
            report.print_agents,
        )
        .await
    {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(error) => config_store_error_response(&error),
    }
}

/// The most envelopes one `POST /sync/stores/{store_id}/events` may carry
/// ([ADR-0143](../../../docs/adr/0143-the-device-credential-syncs-and-events-travel-over-https.md)).
pub const MAX_PUBLISHED_EVENTS: usize = 256;

/// The largest body that route reads, 4 MiB (ADR-0143). A bigger one is answered `413` before it is
/// parsed.
pub const MAX_PUBLISHED_EVENTS_BYTES: usize = 4 * 1024 * 1024;

/// `POST /sync/stores/{store_id}/events`'s body: the store's committed events, oldest first.
#[derive(Debug, Deserialize, utoipa::ToSchema)]
struct PublishedEvents {
    events: Vec<EventEnvelope<RawPayload>>,
}

/// What a publish came to: how many envelopes, counted from the first, the cloud holds. Always the
/// whole batch on success, since ingest commits all of it or none (ADR-0143).
#[derive(Debug, Serialize, utoipa::ToSchema)]
struct PublishedEventsAccepted {
    accepted: usize,
}

#[utoipa::path(
    post,
    path = "/sync/stores/{store_id}/events",
    security(("device_credential" = ["publish_events"])),
    params(
        ("store_id" = String, Path, description = "The store, a ULID. It must be the credential's own"),
    ),
    request_body(
        content = PublishedEvents, description = "`events`: the store's committed event envelopes, oldest first, at most 256 and 4 MiB, \
                       each naming this store (ADR-0143)",
        content_type = "application/json",
    ),
    responses(
        (status = 200, body = PublishedEventsAccepted, description = "`accepted`: how many envelopes the cloud holds, which is the whole \
                                      batch. Ingest is idempotent, so a batch published twice is stored \
                                      once"),
        (status = 400, description = "store_id is not a ULID, the body is not a batch of events, or it \
                                      carries more than 256", body = pos_cloud_api::error_schema::ErrorResponse),
        (status = 401, description = "No credential, or one the cloud does not accept: one answer for every \
                                      credential problem", body = pos_cloud_api::error_schema::ErrorResponse),
        (status = 403, description = "The credential lacks the `publish_events` scope or belongs to another \
                                      store, or an envelope names another store; the batch is refused whole", body = pos_cloud_api::error_schema::ErrorResponse),
        (status = 413, description = "The body is larger than 4 MiB", body = pos_cloud_api::error_schema::ErrorResponse),
        (status = 429, description = "Too many requests to the store sync surface: `Retry-After` says when \
                                      to try again", body = pos_cloud_api::error_schema::ErrorResponse),
        (status = 503, description = "The credential store or the event store is unavailable, and nothing \
                                      was committed", body = pos_cloud_api::error_schema::ErrorResponse),
    ),
    tag = "events",
)]
/// `POST /sync/stores/{store_id}/events` — a store publishes its own events over HTTPS
/// ([ADR-0143](../../../docs/adr/0143-the-device-credential-syncs-and-events-travel-over-https.md)).
///
/// Scope `publish_events`, which a box's device credential always carries, and the store in the
/// path must be the credential's. **Every envelope must name that store**: a batch with any other
/// store in it is refused whole, so no credential can write another shop's history. Ingest is the
/// same idempotent, all-or-nothing [`Cloud::ingest`] the NATS cursor uses, so a batch published
/// twice is stored once and the answer is the whole batch either way.
pub(in crate::http) async fn edge_publish_events<S, R, K, C, A, T, W>(
    State(app): State<CloudApp<S, R, K, C, A, T, W>>,
    headers: HeaderMap,
    Path(store_id): Path<String>,
    body: axum::body::Bytes,
) -> Response
where
    S: EventStore + Clone + Send + Sync + 'static,
    S::Tx: Send,
    R: Clone + Send + Sync + 'static,
    K: ApiKeyStore + Clone + Send + Sync + 'static,
    C: ClockSource + Clone + Send + Sync + 'static,
    A: Clone + Send + Sync + 'static,
    T: Clone + Send + Sync + 'static,
    W: Clone + Send + Sync + 'static,
{
    let store_id = match publishing_store(&app, &headers, &store_id).await {
        Ok(store_id) => store_id,
        Err(refusal) => return refusal,
    };
    let batch = match serde_json::from_slice::<PublishedEvents>(&body) {
        Ok(batch) => batch,
        Err(error) => {
            return api_error_with_details(
                ErrorStatus::InvalidArgument,
                format!("the body is not a batch of events: {error}"),
                &[("events", "INVALID_VALUE")],
            );
        }
    };
    if batch.events.len() > MAX_PUBLISHED_EVENTS {
        return api_error_with_details(
            ErrorStatus::InvalidArgument,
            format!("a batch carries at most {MAX_PUBLISHED_EVENTS} events"),
            &[("events", "TOO_MANY")],
        );
    }
    if batch.events.iter().any(|event| event.store_id != store_id) {
        return api_error(
            ErrorStatus::PermissionDenied,
            "every event in a batch must belong to the store it is published for",
        );
    }
    match app.cloud.ingest(&batch.events).await {
        Ok(_outcome) => (
            StatusCode::OK,
            Json(PublishedEventsAccepted {
                accepted: batch.events.len(),
            }),
        )
            .into_response(),
        Err(error) => error_response(&error),
    }
}

#[utoipa::path(
    post,
    path = "/sync/stores/{store_id}/events/hello",
    security(("device_credential" = ["publish_events"])),
    params(
        ("store_id" = String, Path, description = "The store, a ULID. It must be the credential's own"),
    ),
    request_body(
        content = pos_proto::protocol::Hello, description = "The store's protocol `Hello`, naming this store and the protocol versions it speaks \
                       (ADR-0024)",
        content_type = "application/json",
    ),
    responses(
        (status = 200, body = pos_proto::protocol::HelloOutcome, description = "The negotiation's outcome, an agreed version or a refusal. A refusal \
                                      is an answer, not an error, and the store keeps trading on it"),
        (status = 400, description = "store_id is not a ULID, or the body is not a hello", body = pos_cloud_api::error_schema::ErrorResponse),
        (status = 401, description = "No credential, or one the cloud does not accept: one answer for every \
                                      credential problem", body = pos_cloud_api::error_schema::ErrorResponse),
        (status = 403, description = "The credential lacks the `publish_events` scope or belongs to another \
                                      store, or the hello names another store", body = pos_cloud_api::error_schema::ErrorResponse),
        (status = 429, description = "Too many requests to the store sync surface: `Retry-After` says when \
                                      to try again", body = pos_cloud_api::error_schema::ErrorResponse),
        (status = 503, description = "The credential store is unavailable", body = pos_cloud_api::error_schema::ErrorResponse),
    ),
    tag = "events",
)]
/// `POST /sync/stores/{store_id}/events/hello` — the protocol negotiation for the HTTPS event link
/// ([ADR-0024](../../../docs/adr/0024-protocol-version-negotiation.md), ADR-0143), run on the cloud's side with
/// the cloud's own window. Answers `200` with the [`HelloOutcome`](pos_proto::protocol::HelloOutcome)
/// either way: a refusal is an answer, not an error, and the store keeps trading on it.
pub(in crate::http) async fn edge_events_hello<S, R, K, C, A, T, W>(
    State(app): State<CloudApp<S, R, K, C, A, T, W>>,
    headers: HeaderMap,
    Path(store_id): Path<String>,
    body: axum::body::Bytes,
) -> Response
where
    S: Clone + Send + Sync + 'static,
    R: Clone + Send + Sync + 'static,
    K: ApiKeyStore + Clone + Send + Sync + 'static,
    C: ClockSource + Clone + Send + Sync + 'static,
    A: Clone + Send + Sync + 'static,
    T: Clone + Send + Sync + 'static,
    W: Clone + Send + Sync + 'static,
{
    let store_id = match publishing_store(&app, &headers, &store_id).await {
        Ok(store_id) => store_id,
        Err(refusal) => return refusal,
    };
    let hello = match serde_json::from_slice::<pos_proto::protocol::Hello>(&body) {
        Ok(hello) => hello,
        Err(error) => {
            return api_error(
                ErrorStatus::InvalidArgument,
                format!("the body is not a hello: {error}"),
            );
        }
    };
    if hello.store_id != store_id {
        return api_error(
            ErrorStatus::PermissionDenied,
            "the hello names a store other than the one it is sent for",
        );
    }
    let outcome = pos_proto::protocol::negotiate(
        &hello,
        pos_proto::MIN_SUPPORTED_PROTOCOL_VERSION,
        pos_proto::PROTOCOL_VERSION,
    );
    (StatusCode::OK, Json(outcome)).into_response()
}

/// The checks both event-link routes share: an authenticated caller holding `publish_events`, for
/// the store named in the path, which must be the caller's own.
async fn publishing_store<S, R, K, C, A, T, W>(
    app: &CloudApp<S, R, K, C, A, T, W>,
    headers: &HeaderMap,
    store_id: &str,
) -> Result<StoreId, Response>
where
    K: ApiKeyStore + Send + Sync,
    C: ClockSource + Send + Sync,
{
    let grant = authenticate(&app.keys, &app.clock, headers)
        .await
        .map_err(IntoResponse::into_response)?;
    require_scope(&grant, Scope::PublishEvents).map_err(IntoResponse::into_response)?;
    let store_id = match parse_ulid_fields([("store_id", store_id)]) {
        Ok([store_id]) => StoreId::new(store_id),
        Err(refusal) => return Err(refusal),
    };
    require_store(&grant, store_id).map_err(IntoResponse::into_response)?;
    Ok(store_id)
}

/// What a heartbeat may carry beyond "I am here" ([ADR-0068](../../../docs/adr/0068-fleet-liveness.md)).
///
/// Every field is optional and the whole body is optional, because the body is younger than the
/// route: a store running an older binary posts nothing at all, and must keep being recorded as
/// alive rather than refused for sending the empty body it has always sent.
#[derive(Debug, Default, serde::Deserialize, utoipa::ToSchema)]
struct HeartbeatBody {
    /// How many events the store has committed and not yet published — its own publish backlog, the
    /// opposite direction from the relay backlog the fleet row already carries. `None` when the store
    /// did not say, which is a different answer from zero and leaves the recorded depth alone.
    #[serde(default)]
    outbox_depth: Option<u64>,
    /// The lease generation the box holds
    /// ([ADR-0108](../../../docs/adr/0108-the-lease-generation-is-authority.md)), so the console can
    /// tell a store that has been *replaced* from one that is merely quiet. `None` when the store did
    /// not say — an older edge, or one whose lease row could not be read — and that must not be
    /// recorded as `0`, because `0` is a store's real first generation.
    #[serde(default)]
    lease_generation: Option<u64>,
    /// One standing per bound print agent ([ADR-0112](../../../docs/adr/0112-print-agents.md)), or
    /// `None` when the store said nothing about them — an older edge, or one whose record could not
    /// be read.
    ///
    /// The distinction between absent and empty is load-bearing and is why this is not a bare
    /// `Vec`. Absent leaves whatever the cloud last knew alone. Empty says *this store has no bound
    /// agent*, which a manager releasing the last terminal produces and which must replace the
    /// stored list — a console still showing an agent nobody is bound to is the stale answer this
    /// field exists to prevent.
    #[serde(default)]
    print_agents: Option<Vec<PrintAgentStanding>>,
}

/// Reads a heartbeat's optional body. An empty body is the older edge and yields the default —
/// nothing reported, which the store layer keeps distinct from a reported zero.
///
/// # Errors
///
/// The decode error, for the caller to phrase as a refusal.
fn heartbeat_body(body: &[u8]) -> Result<HeartbeatBody, serde_json::Error> {
    if body.iter().all(u8::is_ascii_whitespace) {
        return Ok(HeartbeatBody::default());
    }
    serde_json::from_slice(body)
}
