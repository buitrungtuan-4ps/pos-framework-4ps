// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The X or Z report for one store's trading day, with each till named from the store's published
//! `devices` node
//! ([ADR-0167](../../../docs/adr/0167-a-till-has-its-own-cash-drawer.md) decision 13).

use axum::Json;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use serde::Deserialize;

use pos_proto::determinism::ClockSource;
use pos_proto::ids::{StoreId, TenantId};

use crate::dashboard::{RollupStore, xz_report};
use crate::refusal::rollup_error_response;
use crate::state::ReportsState;
use crate::till_names::TillNames;
use pos_cloud_api::auth::admin::AdminStore;
use pos_cloud_api::auth::console_rbac::ConsolePermission;
use pos_cloud_api::console::require_permission;
use pos_cloud_api::response::parse_ulid_fields;

#[utoipa::path(
    get,
    path = "/admin/stores/{store_id}/reports/xz",
    extensions(("x-permission" = json!("console.reports.revenue"))),
    security(("session_cookie" = [])),
    params(
        ("store_id" = String, Path, description = "The store (a 26-character ULID)"),
        ("tenant_id" = String, Query, description = "The tenant that store belongs to (a 26-character ULID)"),
        ("business_date" = Option<String>, Query, description = "The trading day, `YYYY-MM-DD`. \
                                                               Absent, the store's latest day"),
    ),
    responses(
        (status = 200, description = "The day's report: `kind`, `X` for the current day (interim) \
                                      or `Z` for a past one (final); `business_date`; `activity`, the \
                                      day's event counts; `revenue`, its takings and mix; `cash`, its \
                                      drawers; and `till_names`, each till's name from the store's \
                                      published devices, which are device names, never a person"),
        (status = 400, description = "tenant_id or store_id is absent or not a ULID", body = pos_cloud_api::error_schema::ErrorResponse),
        (status = 401, description = "No session", body = pos_cloud_api::error_schema::ErrorResponse),
        (status = 403, description = "The role lacks console.reports.revenue", body = pos_cloud_api::error_schema::ErrorResponse),
        (status = 503, description = "The rollup store is unreachable", body = pos_cloud_api::error_schema::ErrorResponse),
    ),
    tag = "reports",
)]
/// An **X or Z report** for one store's trading day (ADR-0081, resolving spec gap D10). `business_date`
/// is optional — absent, the current (latest) day is reported as an X; a past day reads back as a Z.
/// T2 (it bundles revenue and cash), so gated behind `console.reports.revenue`. Each till among the
/// day's drawers is named from the store's published `devices` node as the report is served
/// ([`TillNames`]).
pub async fn admin_xz_report<R, K, C, A, T>(
    State(app): State<ReportsState<R, K, C, A, T>>,
    headers: HeaderMap,
    Path(store_id): Path<String>,
    Query(query): Query<XzReportQuery>,
) -> Response
where
    R: RollupStore + Clone + Send + Sync + 'static,
    K: Clone + Send + Sync + 'static,
    C: ClockSource + Clone + Send + Sync + 'static,
    A: AdminStore + Clone + Send + Sync + 'static,
    T: TillNames + Clone + Send + Sync + 'static,
{
    if let Err(denied) = require_permission(
        &app.admin,
        &app.clock,
        &headers,
        ConsolePermission::ReadRevenue,
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
    match xz_report(&app.rollups, tenant_id, store_id, query.business_date).await {
        Ok(mut report) => {
            report.till_names = app
                .till_names
                .till_names((tenant_id, store_id), &report.cash)
                .await;
            (StatusCode::OK, Json(report)).into_response()
        }
        Err(error) => rollup_error_response(&error),
    }
}

/// The X/Z report query: the explicit `tenant_id` plus an optional `business_date` (`YYYY-MM-DD`);
/// absent, the current day is reported.
#[derive(Debug, Clone, Deserialize)]
pub struct XzReportQuery {
    tenant_id: String,
    #[serde(default)]
    business_date: Option<String>,
}
