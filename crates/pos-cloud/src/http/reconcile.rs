// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! Reconciliation (`/sync/stores/{store_id}/reconcile`, `/internal/reconcile`,
//! `/admin/reconcile`): a store's manifest against the cloud's, and the runs it has recorded.

use axum::Router;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use pos_cloud_api::console::{mint_ulid, require_permission};
use pos_cloud_api::response::{
    api_error_with_details, parse_ulid_fields, service_unavailable, ulid_refusal,
};
use pos_proto::ErrorStatus;
use pos_proto::determinism::ClockSource;
use pos_proto::ids::{EventId, StoreId, TenantId};
use pos_proto::ulid::Ulid;
use serde::Deserialize;

use crate::auth::admin::AdminStore;
use crate::auth::apikey::{ApiKeyStore, Scope};
use crate::auth::bearer::{authenticate, require_scope, require_store};
use crate::auth::console_rbac::ConsolePermission;
use crate::config::InternalSecret;
use crate::http::{Json, internal_guard};
use crate::reconcile::{ReconcileRun, ReconcileRunStore, ReconcileStore};

/// The collaborators the reconciliation routes share: the diff-and-history store, the admin store the
/// admin read authenticates against, the clock that stamps each recorded run and drives the session
/// guard, and the scoped keys the store-facing route authenticates a box with.
#[derive(Clone)]
struct ReconcileState<Rec, A, C, K> {
    store: Rec,
    admin: A,
    clock: C,
    /// The `/internal` shared secret (ADR-0097). It guards the **handler**, not this router: the
    /// same router serves `/admin/reconcile`, which is behind a console permission and must not
    /// start demanding an internal header.
    internal_shared_secret: Option<InternalSecret>,
    /// The scoped keys the **store-facing** route authenticates with, so a box that is off the
    /// cloud's own network can reconcile at all (production-readiness **R3**).
    keys: K,
}

/// Builds the reconciliation sub-router, stated independently of [`CloudApp`](crate::http::CloudApp).
///
/// Reconciliation is the cloud's half of ADR-0040's missing-id diff: a store sends the ids it holds
/// over some window, and the cloud answers with the subset it lacks — the ids to re-push. Every diff
/// also records a run into the history ([ADR-0078](../../../docs/adr/0078-sync-and-ota-closure.md)),
/// so `GET /admin/reconcile` can show the console that reconciliation happened and what it caught.
///
/// # Two doors, because a store is not on the cloud's network
///
/// `POST /sync/stores/{store_id}/reconcile` is the **store's** door, authenticated by its own scoped
/// key and bound to its own store, exactly like the config pull, the heartbeat and the OTA report
/// beside it. It exists because ADR-0040 called reconciliation *edge-initiated* and put it on
/// `/internal/*` — and the shipped proxy denies `/internal/*` to every off-box caller, which a store
/// is by definition. The deferred edge poller would have been written against a route it could never
/// reach (production-readiness **R3**); this is the third route to make that same move.
///
/// `POST /internal/reconcile` stays for a caller that genuinely is on the cloud's own network — an
/// operator's one-off, or a future cloud-side sweep. It names its tenant and store in the body and is
/// guarded by the `/internal` shared secret (ADR-0097); the store's door takes neither, because its
/// identity comes from the key it presents.
///
/// The admin read is behind [`ConsolePermission::Read`]. Stated independently and merged in `main`,
/// rather than threading an extra collaborator through every `CloudApp` handler.
pub fn reconcile_router<Rec, A, C, K>(
    store: Rec,
    admin: A,
    clock: C,
    internal_shared_secret: Option<InternalSecret>,
    keys: K,
) -> Router
where
    Rec: ReconcileStore + ReconcileRunStore + Clone + Send + Sync + 'static,
    A: AdminStore + Clone + Send + Sync + 'static,
    C: ClockSource + Clone + Send + Sync + 'static,
    K: ApiKeyStore + Clone + Send + Sync + 'static,
{
    Router::new()
        .route(
            "/sync/stores/{store_id}/reconcile",
            post(store_reconcile::<Rec, A, C, K>),
        )
        .route("/internal/reconcile", post(reconcile::<Rec, A, C, K>))
        .route(
            "/admin/reconcile",
            get(admin_reconcile_runs::<Rec, A, C, K>),
        )
        .with_state(ReconcileState {
            store,
            admin,
            clock,
            internal_shared_secret,
            keys,
        })
}

/// An edge's reconciliation manifest: the ids it holds for one store, for the cloud to diff.
#[derive(Debug, Clone, Deserialize)]
struct ReconcileRequest {
    /// The tenant the store belongs to (a 26-character ULID).
    tenant_id: String,
    /// The store whose log is being reconciled (a 26-character ULID).
    store_id: String,
    /// The event ids the edge holds for this store over the window it is reconciling.
    event_ids: Vec<String>,
}

/// A store's own reconciliation manifest: just the ids, because the tenant and the store come from
/// the key it presented and the path it called (production-readiness **R3**).
///
/// Deliberately **not** [`ReconcileRequest`] with two ignored members: a body that carries a
/// `tenant_id` the server discards is a body a caller will eventually believe is honoured.
#[derive(Debug, Clone, Deserialize)]
struct StoreReconcileRequest {
    /// The event ids this store holds over the window it is reconciling.
    event_ids: Vec<String>,
}

/// The ids the cloud is missing from the manifest — what the edge should re-push.
#[derive(Debug, Clone, serde::Serialize)]
struct ReconcileResponse {
    /// The subset of the manifest the cloud's event log does not contain (ULID strings).
    missing: Vec<String>,
}

/// Answers which of the edge's candidate ids the cloud is missing for a store
/// ([ADR-0040](../../../docs/adr/0040-reconciliation.md)), and records the run into the history
/// ([ADR-0078](../../../docs/adr/0078-sync-and-ota-closure.md)). Internal (the reconciliation partner of
/// `/internal/ingest`), so it is absent from the public OpenAPI and requires the `X-Pos-Internal-Key`
/// shared secret ([ADR-0097](../../../docs/adr/0097-internal-route-authentication.md)) on top of the
/// proxy denies both deploy lanes apply.
async fn reconcile<Rec, A, C, K>(
    State(state): State<ReconcileState<Rec, A, C, K>>,
    headers: HeaderMap,
    Json(request): Json<ReconcileRequest>,
) -> Response
where
    Rec: ReconcileStore + ReconcileRunStore + Clone + Send + Sync + 'static,
    A: AdminStore + Clone + Send + Sync + 'static,
    C: ClockSource + Clone + Send + Sync + 'static,
    K: ApiKeyStore + Clone + Send + Sync + 'static,
{
    if let Err(refusal) = internal_guard(state.internal_shared_secret.as_ref(), &headers) {
        return refusal;
    }
    let (tenant_id, store_id) = match parse_ulid_fields([
        ("tenant_id", &request.tenant_id),
        ("store_id", &request.store_id),
    ]) {
        Ok([tenant_id, store_id]) => (TenantId::new(tenant_id), StoreId::new(store_id)),
        Err(refusal) => return refusal,
    };
    run_reconcile(&state, tenant_id, store_id, &request.event_ids).await
}

/// `POST /sync/stores/{store_id}/reconcile` — the **store's** door onto the same diff.
///
/// Identity comes from the scoped key the box presents, not from a body it writes: the tenant is the
/// grant's and the store is the path's, checked against the key's binding. That is the same shape the
/// config pull, the heartbeat and the OTA report have, and it is why this route needs no shared
/// secret — a store cannot reach the cloud's private network, and never could
/// (production-readiness **R3**).
async fn store_reconcile<Rec, A, C, K>(
    State(state): State<ReconcileState<Rec, A, C, K>>,
    headers: HeaderMap,
    Path(store_id): Path<String>,
    Json(request): Json<StoreReconcileRequest>,
) -> Response
where
    Rec: ReconcileStore + ReconcileRunStore + Clone + Send + Sync + 'static,
    A: AdminStore + Clone + Send + Sync + 'static,
    C: ClockSource + Clone + Send + Sync + 'static,
    K: ApiKeyStore + Clone + Send + Sync + 'static,
{
    let grant = match authenticate(&state.keys, &state.clock, &headers).await {
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
    run_reconcile(&state, grant.tenant(), store_id, &request.event_ids).await
}

/// The half both reconcile routes share: parse the manifest, run the diff, record the run.
///
/// Extracted for the reason the OTA report's shared body was — the whole difference between the two
/// routes is meant to be *where identity comes from*, and one body makes that true rather than
/// merely intended. A drift here would be a store and an operator getting different answers to the
/// same question.
async fn run_reconcile<Rec, A, C, K>(
    state: &ReconcileState<Rec, A, C, K>,
    tenant_id: TenantId,
    store_id: StoreId,
    event_ids: &[String],
) -> Response
where
    Rec: ReconcileStore + ReconcileRunStore + Clone + Send + Sync + 'static,
    A: AdminStore + Clone + Send + Sync + 'static,
    C: ClockSource + Clone + Send + Sync + 'static,
    K: ApiKeyStore + Clone + Send + Sync + 'static,
{
    let request_event_ids = event_ids;
    let mut candidates = Vec::with_capacity(request_event_ids.len());
    for raw in request_event_ids {
        match raw.parse::<EventId>() {
            Ok(id) => candidates.push(id),
            Err(_) => {
                return api_error_with_details(
                    ErrorStatus::InvalidArgument,
                    "an event id is not a ULID",
                    &[("event_ids", "NOT_A_ULID")],
                );
            }
        }
    }
    match state
        .store
        .absent_event_ids(tenant_id, store_id, &candidates)
        .await
    {
        Ok(missing) => {
            // Record the run into the history — best-effort, so a recording failure never denies the
            // edge the diff it is waiting on (the diff is the primary product; the history is a trail).
            let now = state.clock.now();
            if let Some(run_id) = mint_ulid(now.as_milliseconds_since_epoch()) {
                let run = ReconcileRun {
                    run_id: run_id.to_string(),
                    store: store_id,
                    candidates_offered: u32::try_from(candidates.len()).unwrap_or(u32::MAX),
                    missing_found: u32::try_from(missing.len()).unwrap_or(u32::MAX),
                    ran_at: now,
                };
                if let Err(error) = state.store.record_run(tenant_id, &run).await {
                    tracing::warn!(%error, "recording a reconciliation run failed; the diff still answered");
                }
            }
            (
                StatusCode::OK,
                Json(ReconcileResponse {
                    missing: missing.iter().map(ToString::to_string).collect(),
                }),
            )
                .into_response()
        }
        Err(error) => {
            tracing::error!(%error, "a reconciliation diff failed");
            service_unavailable("reconciliation")
        }
    }
}

/// The default and maximum page size for the reconciliation history read.
const RECONCILE_HISTORY_DEFAULT_LIMIT: u32 = 100;
const RECONCILE_HISTORY_MAX_LIMIT: u32 = 500;

/// A `GET /admin/reconcile` query: the tenant whose runs to list, an optional store filter, and an
/// optional page size.
#[derive(Debug, Clone, Deserialize)]
struct ReconcileHistoryQuery {
    /// The tenant whose reconciliation history to read (a 26-character ULID).
    tenant_id: String,
    /// Narrow to one store (a ULID); absent reads across the tenant's stores.
    #[serde(default)]
    store_id: Option<String>,
    /// Cap the number of runs returned; defaults to [`RECONCILE_HISTORY_DEFAULT_LIMIT`], clamped to
    /// [`RECONCILE_HISTORY_MAX_LIMIT`].
    #[serde(default)]
    limit: Option<u32>,
}

/// One reconciliation run on the wire ([ADR-0078](../../../docs/adr/0078-sync-and-ota-closure.md)):
/// counts and a timestamp, never event contents or a customer identifier.
#[derive(Debug, Clone, serde::Serialize)]
struct ReconcileRunView {
    /// The run's id (a ULID string; chronological).
    run_id: String,
    /// The store the diff was for (a ULID string).
    store_id: String,
    /// How many ids the edge offered in its manifest.
    candidates_offered: u32,
    /// How many of them the cloud was missing (asked the edge to re-push); zero means fully in sync.
    missing_found: u32,
    /// Unix ms of the diff.
    ran_at_ms: i64,
}

impl ReconcileRunView {
    fn from_run(run: ReconcileRun) -> Self {
        Self {
            run_id: run.run_id,
            store_id: run.store.to_string(),
            candidates_offered: run.candidates_offered,
            missing_found: run.missing_found,
            ran_at_ms: run.ran_at.as_milliseconds_since_epoch(),
        }
    }
}

#[utoipa::path(
    get,
    path = "/admin/reconcile",
    security(("session_cookie" = [])),
    params(
        ("tenant_id" = String, Query, description = "The tenant whose runs to list (a 26-character ULID)"),
        ("store_id" = Option<String>, Query, description = "Only this store's runs (a 26-character ULID)"),
        ("limit" = Option<u32>, Query, description = "The most runs to return (default 100, at most 500)"),
    ),
    responses(
        (status = 200, description = "The runs, newest first, as an array: each with `run_id`, \
                                      `store_id`, `candidates_offered` (the events the store \
                                      offered), `missing_found` (how many of them the cloud lacked \
                                      and asked for again; zero is in sync) and `ran_at_ms`"),
        (status = 400, description = "tenant_id is absent or not a ULID, or store_id is not a ULID", body = crate::openapi_admin::ErrorResponse),
        (status = 401, description = "No session", body = crate::openapi_admin::ErrorResponse),
        (status = 403, description = "The role lacks console.read", body = crate::openapi_admin::ErrorResponse),
        (status = 503, description = "The reconciliation store is unreachable", body = crate::openapi_admin::ErrorResponse),
    ),
    tag = "operations",
)]
/// Lists a tenant's most recent reconciliation runs, newest first
/// ([ADR-0078](../../../docs/adr/0078-sync-and-ota-closure.md)) — so the console shows that
/// reconciliation ran and what it caught. Tenant-scoped (a store's runs are its tenant's data), behind
/// [`ConsolePermission::Read`].
async fn admin_reconcile_runs<Rec, A, C, K>(
    State(state): State<ReconcileState<Rec, A, C, K>>,
    headers: HeaderMap,
    Query(query): Query<ReconcileHistoryQuery>,
) -> Response
where
    Rec: ReconcileStore + ReconcileRunStore + Clone + Send + Sync + 'static,
    A: AdminStore + Clone + Send + Sync + 'static,
    C: ClockSource + Clone + Send + Sync + 'static,
    K: ApiKeyStore + Clone + Send + Sync + 'static,
{
    if let Err(denied) = require_permission(
        &state.admin,
        &state.clock,
        &headers,
        ConsolePermission::Read,
    )
    .await
    {
        return denied;
    }
    let tenant_id = match parse_ulid_fields([("tenant_id", &query.tenant_id)]) {
        Ok([tenant_id]) => TenantId::new(tenant_id),
        Err(refusal) => return refusal,
    };
    let store = match query.store_id.as_deref() {
        Some(raw) => match raw.parse::<Ulid>().map(StoreId::new) {
            Ok(store) => Some(store),
            Err(_) => return ulid_refusal(&["store_id"]),
        },
        None => None,
    };
    let limit = query
        .limit
        .unwrap_or(RECONCILE_HISTORY_DEFAULT_LIMIT)
        .min(RECONCILE_HISTORY_MAX_LIMIT);
    match state.store.list_runs(tenant_id, store, limit).await {
        Ok(runs) => {
            let view: Vec<ReconcileRunView> =
                runs.into_iter().map(ReconcileRunView::from_run).collect();
            (StatusCode::OK, Json(view)).into_response()
        }
        Err(error) => {
            tracing::error!(%error, "reading the reconciliation history failed");
            service_unavailable("reconciliation")
        }
    }
}
