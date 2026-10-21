// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! `GET /admin/chain-findings`: what recomputing the chain behind each anchor found (ADR-0132).

use std::sync::Arc;

use axum::Router;
use axum::extract::{Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use pos_cloud_api::console::require_permission;
use pos_cloud_api::response::{parse_ulid_fields, service_unavailable, ulid_refusal};
use pos_proto::determinism::ClockSource;
use pos_proto::ids::{StoreId, TenantId};
use pos_proto::ulid::Ulid;
use serde::Deserialize;

use crate::anchor::{AnchorConflict, AnchorLedger};
use crate::auth::admin::AdminStore;
use crate::auth::console_rbac::ConsolePermission;
use crate::http::Json;

/// The collaborators the chain-finding read shares: the anchor ledger it lists from, the admin store
/// the permission check authenticates against, and the clock that drives the session guard.
#[derive(Clone)]
struct ChainState<A, C> {
    anchors: Arc<dyn AnchorLedger>,
    admin: A,
    clock: C,
}

/// The default and maximum page size for the chain-finding read.
const CHAIN_FINDINGS_DEFAULT_LIMIT: u32 = 100;
const CHAIN_FINDINGS_MAX_LIMIT: u32 = 500;

/// Builds the chain-finding sub-router, stated independently of [`CloudApp`](crate::http::CloudApp).
///
/// One route: `GET /admin/chain-findings`, the contradictions the cloud refused
/// ([ADR-0131](../../../docs/adr/0131-a-chained-event-log.md) decision 4,
/// [ADR-0132](../../../docs/adr/0132-the-cloud-recomputes-the-chain-it-holds.md)). Until this
/// existed a finding reached a human only as a line in the server's log and a row nobody could
/// read — which for a mechanism whose entire product is *being noticed* is most of the way to not
/// having it.
///
/// Stated independently and merged in `main`, like [`reconcile_router`](crate::http::reconcile_router), rather than threading an
/// extra collaborator through every [`CloudApp`](crate::http::CloudApp) handler. The ledger is an `Arc<dyn AnchorLedger>`
/// because that is what [`crate::cloud::Cloud`] already holds one as.
pub fn chain_router<A, C>(anchors: Arc<dyn AnchorLedger>, admin: A, clock: C) -> Router
where
    A: AdminStore + Clone + Send + Sync + 'static,
    C: ClockSource + Clone + Send + Sync + 'static,
{
    Router::new()
        .route("/admin/chain-findings", get(admin_chain_findings::<A, C>))
        .with_state(ChainState {
            anchors,
            admin,
            clock,
        })
}

/// A `GET /admin/chain-findings` query: the tenant whose findings to list, an optional store filter,
/// and an optional page size.
#[derive(Debug, Clone, Deserialize)]
struct ChainFindingsQuery {
    /// The tenant whose findings to read (a 26-character ULID).
    tenant_id: String,
    /// Narrow to one store (a ULID); absent reads across the tenant's stores.
    #[serde(default)]
    store_id: Option<String>,
    /// Cap the number of findings returned; defaults to [`CHAIN_FINDINGS_DEFAULT_LIMIT`], clamped to
    /// [`CHAIN_FINDINGS_MAX_LIMIT`].
    #[serde(default)]
    limit: Option<u32>,
}

/// One refused chain claim on the wire.
///
/// Positions, hashes and identifiers — no event contents and no customer identifier. A chain hash is
/// a digest of an envelope whose every payload field `pos_proto::pii` has already proven free of
/// personal data.
#[derive(Debug, Clone, serde::Serialize)]
struct ChainFindingView {
    /// The finding's id, derived from the contradiction rather than minted, so a re-delivery of the
    /// same bad claim does not multiply into rows.
    finding_id: String,
    /// The store whose log disagrees with itself (a ULID string).
    store_id: String,
    /// The chain length both hashes claim to describe.
    chain_seq: u64,
    /// What the cloud holds there.
    held_head: String,
    /// What it was offered against it.
    offered_head: String,
    /// The event that carried the offered claim, so the finding traces back to a record.
    offered_event_id: String,
    /// Unix ms when the **cloud** noticed. The server's clock, which is the one an investigation can
    /// rely on — the store's is on the anchor, and a store that is tampering controls that one.
    noticed_at_ms: i64,
}

impl ChainFindingView {
    fn from_conflict(conflict: AnchorConflict) -> Self {
        Self {
            finding_id: conflict.conflict_id,
            store_id: conflict.report.store.to_string(),
            chain_seq: conflict.report.chain_seq,
            held_head: conflict.report.held_head.as_str().to_owned(),
            offered_head: conflict.report.offered_head.as_str().to_owned(),
            offered_event_id: conflict.report.offered_event_id.to_string(),
            noticed_at_ms: conflict.noticed_at.as_milliseconds_since_epoch(),
        }
    }
}

#[utoipa::path(
    get,
    path = "/admin/chain-findings",
    security(("session_cookie" = [])),
    extensions(("x-permission" = json!("console.data.read"))),
    params(
        ("tenant_id" = String, Query, description = "The tenant whose findings to read (a 26-character ULID)"),
        ("store_id" = Option<String>, Query, description = "Narrow to one store (a ULID); absent reads across the tenant's stores"),
    ),
    responses(
        (status = 200, description = "The contradictions the cloud refused, newest first. Each is \
                                      two hashes claiming to describe one store's chain at one \
                                      length, and only one of them can be true. An empty list is \
                                      the expected answer: it means nothing was refused, not that \
                                      nothing was checked"),
        (status = 400, description = "tenant_id or store_id is absent or not a ULID", body = crate::openapi_admin::ErrorResponse),
        (status = 401, description = "No session", body = crate::openapi_admin::ErrorResponse),
        (status = 403, description = "The role lacks console.data.read", body = crate::openapi_admin::ErrorResponse),
        (status = 503, description = "The anchor ledger is unreachable", body = crate::openapi_admin::ErrorResponse),
    ),
    tag = "fleet",
)]
/// Lists a tenant's most recent refused chain claims, newest first. Tenant-scoped (a store's
/// findings are its tenant's data), behind [`ConsolePermission::Read`].
async fn admin_chain_findings<A, C>(
    State(state): State<ChainState<A, C>>,
    headers: HeaderMap,
    Query(query): Query<ChainFindingsQuery>,
) -> Response
where
    A: AdminStore + Clone + Send + Sync + 'static,
    C: ClockSource + Clone + Send + Sync + 'static,
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
        .unwrap_or(CHAIN_FINDINGS_DEFAULT_LIMIT)
        .min(CHAIN_FINDINGS_MAX_LIMIT);
    match state.anchors.list_conflicts(tenant_id, store, limit).await {
        Ok(conflicts) => {
            let view: Vec<ChainFindingView> = conflicts
                .into_iter()
                .map(ChainFindingView::from_conflict)
                .collect();
            (StatusCode::OK, Json(view)).into_response()
        }
        Err(error) => {
            tracing::error!(%error, "reading the chain findings failed");
            service_unavailable("chain findings")
        }
    }
}
