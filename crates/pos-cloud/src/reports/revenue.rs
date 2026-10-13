// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The revenue read and its CSV exports: daily totals, fees by code
//! ([ADR-0159](../../../../docs/adr/0159-a-fee-is-configuration.md)) and tax by component
//! ([ADR-0168](../../../../docs/adr/0168-a-settled-bill-records-its-tax-components.md)). Revenue is
//! T2, so every route here needs `console.reports.revenue`.

use axum::Json;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};

use pos_proto::ErrorStatus;
use pos_proto::determinism::ClockSource;
use pos_proto::ids::{StoreId, TenantId};

use crate::auth::admin::AdminStore;
use crate::auth::console_rbac::ConsolePermission;
use crate::cloud::DailyRevenue;
use crate::dashboard::{RollupStore, RollupWindow, revenue};
use crate::export;
use crate::http::{
    CloudApp, parse_ulid_fields, require_permission, rollup_error_response, window_refusal,
};
use crate::http::{api_error, audit_action, csv_download_response};
use crate::reports::rollups::AdminRollupWindowQuery;

/// The materialised **revenue** rollup for one store, windowed exactly as [`admin_daily_rollups`].
///
/// Prices are **T2** (ADR-0081), so this is gated behind `console.reports.revenue` (Owner/Admin) —
/// narrower than the counts read's `console.data.read`. The rollup carries no customer or employee
/// identifier; the tenant is named explicitly (the console admin is global).
pub(crate) async fn admin_revenue_daily<S, R, K, C, A, T, W>(
    State(app): State<CloudApp<S, R, K, C, A, T, W>>,
    headers: HeaderMap,
    Path(store_id): Path<String>,
    Query(query): Query<AdminRollupWindowQuery>,
) -> Response
where
    S: Clone + Send + Sync + 'static,
    R: RollupStore + Clone + Send + Sync + 'static,
    K: Clone + Send + Sync + 'static,
    C: ClockSource + Clone + Send + Sync + 'static,
    A: AdminStore + Clone + Send + Sync + 'static,
    T: Clone + Send + Sync + 'static,
    W: Clone + Send + Sync + 'static,
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
    let window = match RollupWindow::new(query.from, query.to, query.limit) {
        Ok(window) => window,
        Err(error) => return window_refusal(error),
    };
    match revenue(&app.rollups, tenant_id, store_id, &window).await {
        Ok(revenue) => (StatusCode::OK, Json(revenue)).into_response(),
        Err(error) => rollup_error_response(&error),
    }
}

/// What sets one of a store's revenue exports apart from the others: its file, how it is written,
/// and what its audit entry calls it and counts.
///
/// The exports are one rule, which [`export_revenue`] keeps: revenue is **T2**, so each needs
/// `console.reports.revenue`, reads the window the revenue read does, and is audited by how many
/// rows it held, never what they held.
struct RevenueExport {
    /// The file the download is saved as, such as `revenue-tax.csv`.
    name: &'static str,
    /// The audit entry's action, such as `reports.export_revenue_tax`.
    action: &'static str,
    /// The audit entry's `domain`, such as `revenue_tax`.
    domain: &'static str,
    /// Writes the window's days as the file.
    write: fn(&[DailyRevenue]) -> Result<Vec<u8>, csv::Error>,
    /// How many rows the file holds under its header.
    rows: fn(&[DailyRevenue]) -> usize,
}

/// Downloads a store's daily revenue rollups as `file` writes them, windowed like the read
/// (ADR-0081, Track O4).
async fn export_revenue<S, R, K, C, A, T, W>(
    app: &CloudApp<S, R, K, C, A, T, W>,
    headers: &HeaderMap,
    store_id: &str,
    query: AdminRollupWindowQuery,
    file: &RevenueExport,
) -> Response
where
    S: Clone + Send + Sync + 'static,
    R: RollupStore + Clone + Send + Sync + 'static,
    K: Clone + Send + Sync + 'static,
    C: ClockSource + Clone + Send + Sync + 'static,
    A: AdminStore + Clone + Send + Sync + 'static,
    T: Clone + Send + Sync + 'static,
    W: Clone + Send + Sync + 'static,
{
    let context = match require_permission(
        &app.admin,
        &app.clock,
        headers,
        ConsolePermission::ReadRevenue,
    )
    .await
    {
        Ok(context) => context,
        Err(denied) => return denied,
    };
    let (tenant_id, store_id) =
        match parse_ulid_fields([("tenant_id", &query.tenant_id), ("store_id", store_id)]) {
            Ok([tenant_id, store_id]) => (TenantId::new(tenant_id), StoreId::new(store_id)),
            Err(refusal) => return refusal,
        };
    let window = match RollupWindow::new(query.from, query.to, query.limit) {
        Ok(window) => window,
        Err(error) => return window_refusal(error),
    };
    let days = match revenue(&app.rollups, tenant_id, store_id, &window).await {
        Ok(days) => days,
        Err(error) => return rollup_error_response(&error),
    };
    let Ok(body) = (file.write)(&days) else {
        return api_error(ErrorStatus::Internal, "could not build the CSV");
    };
    audit_action(
        &app.audit,
        &app.clock,
        &context,
        Some(tenant_id),
        file.action,
        "store",
        &store_id.to_string(),
        None,
        Some(serde_json::json!({ "domain": file.domain, "rows": (file.rows)(&days) })),
    )
    .await;
    csv_download_response(file.name, body)
}

/// Exports a store's daily revenue rollups as a CSV download, one row per trading day (ADR-0081,
/// Track O4).
pub(crate) async fn admin_export_revenue<S, R, K, C, A, T, W>(
    State(app): State<CloudApp<S, R, K, C, A, T, W>>,
    headers: HeaderMap,
    Path(store_id): Path<String>,
    Query(query): Query<AdminRollupWindowQuery>,
) -> Response
where
    S: Clone + Send + Sync + 'static,
    R: RollupStore + Clone + Send + Sync + 'static,
    K: Clone + Send + Sync + 'static,
    C: ClockSource + Clone + Send + Sync + 'static,
    A: AdminStore + Clone + Send + Sync + 'static,
    T: Clone + Send + Sync + 'static,
    W: Clone + Send + Sync + 'static,
{
    let file = RevenueExport {
        name: "revenue.csv",
        action: "reports.export_revenue",
        domain: "revenue",
        write: export::revenue_csv,
        rows: <[DailyRevenue]>::len,
    };
    export_revenue(&app, &headers, &store_id, query, &file).await
}

#[utoipa::path(
    get,
    path = "/admin/stores/{store_id}/revenue/fees/export",
    params(
        ("store_id" = String, Path, description = "The store whose fees to export (a 26-character ULID)"),
        ("tenant_id" = String, Query, description = "The tenant that store belongs to (a 26-character ULID)"),
        ("from" = Option<String>, Query, description = "The first trading day, `YYYY-MM-DD`, inclusive"),
        ("to" = Option<String>, Query, description = "The last trading day, `YYYY-MM-DD`, inclusive"),
        ("limit" = Option<usize>, Query, description = "The most trading days to cover, the \
                                                       newest in range: 90 when absent, and \
                                                       never more than 366"),
    ),
    responses(
        (status = 200, description = "`revenue-fees.csv`, as a `text/csv` attachment. The header \
                                      is `business_date,currency_code,fee_code,fee_name,bills,\
                                      amount,tax`, then one row per trading day per fee code the \
                                      day's settled bills charged, oldest day first and then by \
                                      code: the fee's name, the bills that charged it, what it \
                                      charged and the tax on it, in the store's currency's minor \
                                      units. A day that charged no fee has no row, and neither \
                                      has a bill settled by an edge from before fee lines, whose \
                                      charge is only in `revenue.csv`'s `service_charge`. Only \
                                      the header for a window with no fees, and for a store \
                                      whose rollup has not been projected yet"),
        (status = 400, description = "tenant_id or store_id is absent or not a ULID, a date is \
                                      not `YYYY-MM-DD`, `from` is after `to`, or `limit` is \
                                      zero", body = crate::openapi_admin::ErrorResponse),
        (status = 401, description = "No session", body = crate::openapi_admin::ErrorResponse),
        (status = 403, description = "The role lacks console.reports.revenue", body = crate::openapi_admin::ErrorResponse),
        (status = 500, description = "The CSV could not be built", body = crate::openapi_admin::ErrorResponse),
        (status = 503, description = "The rollup store is unreachable", body = crate::openapi_admin::ErrorResponse),
    ),
    tag = "reports",
)]
/// A store's fees by code as a CSV download, one row per trading day per code
/// ([ADR-0159](../../../docs/adr/0159-a-fee-is-configuration.md) decision 4).
///
/// Windowed as the revenue read is, and long-format: a file of its own rather than columns added to
/// `revenue.csv`, whose header stays as every spreadsheet built on it reads it. Prices are **T2**,
/// so it needs `console.reports.revenue`, as `GET /admin/stores/{store_id}/revenue/export` does,
/// and the audit trail records how many rows were exported, never what they held.
pub(crate) async fn admin_export_revenue_fees<S, R, K, C, A, T, W>(
    State(app): State<CloudApp<S, R, K, C, A, T, W>>,
    headers: HeaderMap,
    Path(store_id): Path<String>,
    Query(query): Query<AdminRollupWindowQuery>,
) -> Response
where
    S: Clone + Send + Sync + 'static,
    R: RollupStore + Clone + Send + Sync + 'static,
    K: Clone + Send + Sync + 'static,
    C: ClockSource + Clone + Send + Sync + 'static,
    A: AdminStore + Clone + Send + Sync + 'static,
    T: Clone + Send + Sync + 'static,
    W: Clone + Send + Sync + 'static,
{
    let file = RevenueExport {
        name: "revenue-fees.csv",
        action: "reports.export_revenue_fees",
        domain: "revenue_fees",
        write: export::revenue_fees_csv,
        rows: |days| days.iter().map(|day| day.by_fee.len()).sum(),
    };
    export_revenue(&app, &headers, &store_id, query, &file).await
}

#[utoipa::path(
    get,
    path = "/admin/stores/{store_id}/revenue/tax/export",
    params(
        ("store_id" = String, Path, description = "The store whose tax to export (a 26-character ULID)"),
        ("tenant_id" = String, Query, description = "The tenant that store belongs to (a 26-character ULID)"),
        ("from" = Option<String>, Query, description = "The first trading day, `YYYY-MM-DD`, inclusive"),
        ("to" = Option<String>, Query, description = "The last trading day, `YYYY-MM-DD`, inclusive"),
        ("limit" = Option<usize>, Query, description = "The most trading days to cover, the \
                                                       newest in range: 90 when absent, and \
                                                       never more than 366"),
    ),
    responses(
        (status = 200, description = "`revenue-tax.csv`, as a `text/csv` attachment. The header \
                                      is `business_date,currency_code,component_name,\
                                      rate_basis_points,tax`, then one row per trading day per \
                                      tax component name and rate the day's settled bills \
                                      recorded, oldest day first and then by name and rate: the \
                                      tax their tax lines recorded under it, in the store's \
                                      currency's minor units. A fee's tax is inside its tax \
                                      line, so it adds no row of its own. Nothing is invented: a \
                                      bill that recorded no components, settled before its store \
                                      recorded them or taxed at rates with none, is in \
                                      `revenue.csv`'s `tax` and in no row here, so a day's rows \
                                      can sum to less than its tax and no row stands for the \
                                      difference. Only the header for a window with no \
                                      components, as for a store whose tax table names none, and \
                                      for a store whose rollup has not been projected yet"),
        (status = 400, description = "tenant_id or store_id is absent or not a ULID, a date is \
                                      not `YYYY-MM-DD`, `from` is after `to`, or `limit` is \
                                      zero", body = crate::openapi_admin::ErrorResponse),
        (status = 401, description = "No session", body = crate::openapi_admin::ErrorResponse),
        (status = 403, description = "The role lacks console.reports.revenue", body = crate::openapi_admin::ErrorResponse),
        (status = 500, description = "The CSV could not be built", body = crate::openapi_admin::ErrorResponse),
        (status = 503, description = "The rollup store is unreachable", body = crate::openapi_admin::ErrorResponse),
    ),
    tag = "reports",
)]
/// A store's tax by component as a CSV download, one row per trading day per component name and
/// rate ([ADR-0168](../../../docs/adr/0168-a-settled-bill-records-its-tax-components.md)
/// decision 5).
///
/// A file of its own beside `revenue-fees.csv`, and the same in every way that matters: windowed
/// as the revenue read is, behind `console.reports.revenue`, and audited by how many rows were
/// exported, never what they held.
pub(crate) async fn admin_export_revenue_tax<S, R, K, C, A, T, W>(
    State(app): State<CloudApp<S, R, K, C, A, T, W>>,
    headers: HeaderMap,
    Path(store_id): Path<String>,
    Query(query): Query<AdminRollupWindowQuery>,
) -> Response
where
    S: Clone + Send + Sync + 'static,
    R: RollupStore + Clone + Send + Sync + 'static,
    K: Clone + Send + Sync + 'static,
    C: ClockSource + Clone + Send + Sync + 'static,
    A: AdminStore + Clone + Send + Sync + 'static,
    T: Clone + Send + Sync + 'static,
    W: Clone + Send + Sync + 'static,
{
    let file = RevenueExport {
        name: "revenue-tax.csv",
        action: "reports.export_revenue_tax",
        domain: "revenue_tax",
        write: export::revenue_tax_csv,
        rows: |days| days.iter().map(|day| day.by_tax_component.len()).sum(),
    };
    export_revenue(&app, &headers, &store_id, query, &file).await
}
