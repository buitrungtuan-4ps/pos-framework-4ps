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
