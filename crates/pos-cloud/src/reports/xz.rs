// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The X or Z report for one store's trading day, with each till named from the store's published
//! `devices` node
//! ([ADR-0167](../../../../docs/adr/0167-a-till-has-its-own-cash-drawer.md) decision 13).

use axum::Json;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use serde::Deserialize;

use pos_proto::determinism::ClockSource;
use pos_proto::devices::PublishedDevices;
use pos_proto::ids::{StoreId, TenantId};

use crate::auth::admin::AdminStore;
use crate::auth::console_rbac::ConsolePermission;
use crate::cloud::{DailyCash, STORE_DRAWER};
use crate::config_tree::{CapabilityValidator, ConfigTree, ConfigTreeStore};
use crate::dashboard::{RollupStore, xz_report};
use crate::http::strip_tree_version;
use crate::http::{parse_ulid_fields, require_permission, rollup_error_response};
use crate::reports::state::ReportsState;

/// An **X or Z report** for one store's trading day (ADR-0081, resolving spec gap D10). `business_date`
/// is optional — absent, the current (latest) day is reported as an X; a past day reads back as a Z.
/// T2 (it bundles revenue and cash), so gated behind `console.reports.revenue`. Each till among the
/// day's drawers is named from the store's published `devices` node as the report is served
/// ([`published_till_names`]).
pub(crate) async fn admin_xz_report<R, K, C, A, T>(
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
    T: ConfigTreeStore + Clone + Send + Sync + 'static,
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
            report.till_names =
                published_till_names(&app.config_trees, (tenant_id, store_id), &report.cash).await;
            (StatusCode::OK, Json(report)).into_response()
        }
        Err(error) => rollup_error_response(&error),
    }
}

/// The names the store's published `devices` node gives the tills among `cash`'s drawers, by
/// `terminal_device_id` (ADR-0167 decision 13).
///
/// Read as the report is served, so a till renamed since shows its new name and one the node no
/// longer lists shows none, and the console labels it by its id. Only `TERMINAL` entries name a
/// drawer. A day with only the store's drawer reads nothing, and a tree that cannot be read names
/// nothing rather than failing the report, whose figures stand without the names.
async fn published_till_names<T: ConfigTreeStore>(
    config_trees: &T,
    (tenant_id, store_id): (TenantId, StoreId),
    cash: &DailyCash,
) -> std::collections::BTreeMap<String, String> {
    if cash.by_drawer.keys().all(|drawer| drawer == STORE_DRAWER) {
        return std::collections::BTreeMap::new();
    }
    let Ok(Some(state)) = config_trees
        .load(tenant_id, store_id)
        .await
        .map(strip_tree_version)
    else {
        return std::collections::BTreeMap::new();
    };
    let devices = ConfigTree::from_state(store_id, CapabilityValidator, state)
        .current_effective()
        .and_then(|effective| effective.get("devices").cloned())
        .and_then(|node| serde_json::from_value::<PublishedDevices>(node).ok())
        .unwrap_or_default();
    devices
        .devices()
        .iter()
        .filter(|device| device.kind.known() == pos_proto::devices::DeviceKind::Terminal)
        .map(|till| (till.device_id.to_string(), till.name.as_str().to_owned()))
        .filter(|(till, _)| cash.by_drawer.contains_key(till))
        .collect()
}

/// The X/Z report query: the explicit `tenant_id` plus an optional `business_date` (`YYYY-MM-DD`);
/// absent, the current day is reported.
#[derive(Debug, Clone, Deserialize)]
pub(crate) struct XzReportQuery {
    tenant_id: String,
    #[serde(default)]
    business_date: Option<String>,
}
