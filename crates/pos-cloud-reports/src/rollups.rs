// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The daily rollup reads (`/v1` by API key, `/admin` by session), their CSV export, and the reset
//! that makes the projector rebuild a store's rollup
//! ([ADR-0036](../../../../docs/adr/0036-materialised-rollups.md)).

use axum::Json;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use serde::Deserialize;

use pos_proto::ErrorStatus;
use pos_proto::determinism::ClockSource;
use pos_proto::ids::{StoreId, TenantId};

use crate::dashboard::rollup::WindowError;
use crate::dashboard::{RollupStore, RollupWindow, StoredRollups, dashboard};
use crate::export;
use crate::refusal::{rollup_error_response, window_refusal};
use crate::state::ReportsState;
use crate::types::DailyRollup;
use pos_cloud_api::auth::admin::AdminStore;
use pos_cloud_api::auth::apikey::{ApiKeyStore, Scope};
use pos_cloud_api::auth::bearer::{authenticate, confine_to_store, require_scope};
use pos_cloud_api::auth::console_rbac::ConsolePermission;
use pos_cloud_api::console::{ConfigTenantQuery, audit_action, require_permission};
use pos_cloud_api::response::{api_error, csv_download_response, parse_ulid_fields};

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
pub async fn daily_rollups<R, K, C, A, T>(
    State(app): State<ReportsState<R, K, C, A, T>>,
    headers: HeaderMap,
    Path(store_id): Path<String>,
    Query(window): Query<RollupWindowQuery>,
) -> Response
where
    R: RollupStore + Clone + Send + Sync + 'static,
    K: ApiKeyStore + Clone + Send + Sync + 'static,
    C: ClockSource + Clone + Send + Sync + 'static,
    A: Clone + Send + Sync + 'static,
    T: Clone + Send + Sync + 'static,
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
    let store_id = match parse_ulid_fields([("store_id", &store_id)]) {
        Ok([store_id]) => StoreId::new(store_id),
        Err(refusal) => return refusal,
    };
    if let Err(forbidden) = confine_to_store(&grant, store_id) {
        return forbidden.into_response();
    }
    let window = match window.into_window() {
        Ok(window) => window,
        Err(error) => return window_refusal(error),
    };
    // The tenant is the grant's, not the request's — this is the isolation boundary.
    match dashboard(&app.rollups, grant.tenant(), store_id, &window).await {
        Ok(rollups) => (StatusCode::OK, Json(rollups)).into_response(),
        Err(error) => rollup_error_response(&error),
    }
}

/// The date-range window params shared by the rollup read routes (ADR-0081, Track O4): an inclusive
/// `from`/`to` business-date range and a `limit` cap. All optional — absent gives the default
/// [`crate::dashboard::rollup::DEFAULT_WINDOW_DAYS`]-day window, never the store's whole history.
#[derive(Debug, Clone, Deserialize)]
pub struct RollupWindowQuery {
    #[serde(default)]
    from: Option<String>,
    #[serde(default)]
    to: Option<String>,
    #[serde(default)]
    limit: Option<usize>,
}

impl RollupWindowQuery {
    /// Validates the params into a [`RollupWindow`], or returns a `400` message.
    fn into_window(self) -> Result<RollupWindow, WindowError> {
        RollupWindow::new(self.from, self.to, self.limit)
    }
}

/// The daily rollup for a store, read under the super-admin session (ADR-0060). The `/v1` rollup
/// read is bearer-authed and tenant-scoped by the key; the dashboard carries the admin session, not
/// a tenant key, so this reads the same rollup while naming the tenant with `?tenant_id=`. The
/// super-admin is global ([ADR-0034](../../../../docs/adr/0034-super-admin-auth.md)) — it already reads
/// any tenant's configuration — so this is the same trust boundary, and it is a read: nothing is
/// mutated.
pub async fn admin_daily_rollups<R, K, C, A, T>(
    State(app): State<ReportsState<R, K, C, A, T>>,
    headers: HeaderMap,
    Path(store_id): Path<String>,
    Query(query): Query<AdminRollupWindowQuery>,
) -> Response
where
    R: RollupStore + Clone + Send + Sync + 'static,
    K: Clone + Send + Sync + 'static,
    C: ClockSource + Clone + Send + Sync + 'static,
    A: AdminStore + Clone + Send + Sync + 'static,
    T: Clone + Send + Sync + 'static,
{
    if let Err(denied) =
        require_permission(&app.admin, &app.clock, &headers, ConsolePermission::Read).await
    {
        return denied;
    }
    let (tenant_id, store_id) =
        match parse_ulid_fields([("tenant_id", &query.tenant_id), ("store_id", &store_id)]) {
            Ok([tenant_id, store_id]) => (TenantId::new(tenant_id), StoreId::new(store_id)),
            Err(refusal) => return refusal,
        };
    let window = match RollupWindow::new(query.from, query.to, query.limit) {
        Ok(window) => window,
        Err(error) => return window_refusal(error),
    };
    // The tenant is named explicitly here (the admin is global), unlike the `/v1` read where it is
    // the API key's. It is a read of the materialised rollup — event counts only, no PII.
    match dashboard(&app.rollups, tenant_id, store_id, &window).await {
        Ok(rollups) => (StatusCode::OK, Json(rollups)).into_response(),
        Err(error) => rollup_error_response(&error),
    }
}

/// The `/admin` daily-rollup query: the explicit `tenant_id` (the console admin is global) plus the
/// shared [`RollupWindowQuery`] date range and cap (ADR-0081, Track O4).
#[derive(Debug, Clone, Deserialize)]
pub struct AdminRollupWindowQuery {
    /// The tenant the store belongs to (a 26-character ULID).
    pub tenant_id: String,
    /// The first trading day, `YYYY-MM-DD`, inclusive.
    #[serde(default)]
    pub from: Option<String>,
    /// The last trading day, `YYYY-MM-DD`, inclusive.
    #[serde(default)]
    pub to: Option<String>,
    /// The most trading days to cover.
    #[serde(default)]
    pub limit: Option<usize>,
}

/// Exports a store's daily activity rollups (counts) as a CSV download, windowed like the read
/// (ADR-0081, Track O4; reuses the ADR-0075 rail). Counts only, so `console.data.read`; audited by
/// row count.
pub async fn admin_export_rollups<R, K, C, A, T>(
    State(app): State<ReportsState<R, K, C, A, T>>,
    headers: HeaderMap,
    Path(store_id): Path<String>,
    Query(query): Query<AdminRollupWindowQuery>,
) -> Response
where
    R: RollupStore + Clone + Send + Sync + 'static,
    K: Clone + Send + Sync + 'static,
    C: ClockSource + Clone + Send + Sync + 'static,
    A: AdminStore + Clone + Send + Sync + 'static,
    T: Clone + Send + Sync + 'static,
{
    let context =
        match require_permission(&app.admin, &app.clock, &headers, ConsolePermission::Read).await {
            Ok(context) => context,
            Err(denied) => return denied,
        };
    let (tenant_id, store_id) =
        match parse_ulid_fields([("tenant_id", &query.tenant_id), ("store_id", &store_id)]) {
            Ok([tenant_id, store_id]) => (TenantId::new(tenant_id), StoreId::new(store_id)),
            Err(refusal) => return refusal,
        };
    let window = match RollupWindow::new(query.from, query.to, query.limit) {
        Ok(window) => window,
        Err(error) => return window_refusal(error),
    };
    let days = match dashboard(&app.rollups, tenant_id, store_id, &window).await {
        Ok(days) => days,
        Err(error) => return rollup_error_response(&error),
    };
    let Ok(body) = export::rollups_csv(&days) else {
        return api_error(ErrorStatus::Internal, "could not build the CSV");
    };
    audit_action(
        &app.audit,
        &app.clock,
        &context,
        Some(tenant_id),
        "reports.export_rollups",
        "store",
        &store_id.to_string(),
        None,
        Some(serde_json::json!({ "domain": "rollups", "rows": days.len() })),
    )
    .await;
    csv_download_response("rollups.csv", body)
}

/// Resets a store's materialised rollup so the projector rebuilds it from the event log
/// (super-admin only). Saving the empty default clears the per-store cursor, and the next projector
/// pass re-folds every event from the start — the "reset-cursor-and-replay" that rebuilds the cloud's
/// read model from the durable log (`docs/roadmap.md` P7, [ADR-0036](../../../../docs/adr/0036-materialised-rollups.md)).
/// `204` regardless — a store with no rollup yet is simply reset to the same empty state.
pub async fn admin_rollups_reset<R, K, C, A, T>(
    State(app): State<ReportsState<R, K, C, A, T>>,
    headers: HeaderMap,
    Path(store_id): Path<String>,
    Query(query): Query<ConfigTenantQuery>,
) -> Response
where
    R: RollupStore + Clone + Send + Sync + 'static,
    K: Clone + Send + Sync + 'static,
    C: ClockSource + Clone + Send + Sync + 'static,
    A: AdminStore + Clone + Send + Sync + 'static,
    T: Clone + Send + Sync + 'static,
{
    if let Err(denied) = require_permission(
        &app.admin,
        &app.clock,
        &headers,
        ConsolePermission::PublishConfig,
    )
    .await
    {
        return denied;
    }
    let (tenant_id, store_id) =
        match parse_ulid_fields([("tenant_id", &query.tenant_id), ("store_id", &store_id)]) {
            Ok([tenant_id, store_id]) => (TenantId::new(tenant_id), StoreId::new(store_id)),
            Err(refusal) => return refusal,
        };
    match app
        .rollups
        .save(tenant_id, store_id, &StoredRollups::default())
        .await
    {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(error) => rollup_error_response(&error),
    }
}
