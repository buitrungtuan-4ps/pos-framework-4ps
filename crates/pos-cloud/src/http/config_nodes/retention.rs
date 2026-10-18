// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! `/admin/config/retention`: how long a store keeps what it holds, read and publish.

use axum::extract::{Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use pos_cloud_api::console::require_permission;
use pos_cloud_api::response::{api_error_with_details, parse_ulid_fields};
use pos_proto::ErrorStatus;
use pos_proto::determinism::ClockSource;
use pos_proto::ids::{StoreId, TenantId};
use pos_proto::retention::EVENT_LOG_DAYS;
use serde::Deserialize;

use super::{ConfigChannelsState, ConfigNodeQuery, publish_store_settings_node};
use crate::auth::admin::AdminStore;
use crate::auth::console_rbac::ConsolePermission;
use crate::config_tree::ConfigTreeStore;
use crate::http::{Json, read_store_node_as_run};

/// A `PUT /admin/config/retention` body: how many days a store's edge keeps a synced event
/// (ADR-0145).
#[derive(Debug, Clone, Deserialize)]
pub(super) struct PublishRetentionRequest {
    tenant_id: String,
    store_id: String,
    event_log_days: u32,
}

/// A super-admin reads a store's current `retention` node, or `null` (the edge then keeps 90 days).
pub(super) async fn admin_read_retention<Cfg, A, C>(
    State(state): State<ConfigChannelsState<Cfg, A, C>>,
    headers: HeaderMap,
    Query(query): Query<ConfigNodeQuery>,
) -> Response
where
    Cfg: ConfigTreeStore + Clone + Send + Sync + 'static,
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
    let (tenant_id, store_id) = match parse_ulid_fields([
        ("tenant_id", &query.tenant_id),
        ("store_id", &query.store_id),
    ]) {
        Ok([tenant_id, store_id]) => (TenantId::new(tenant_id), StoreId::new(store_id)),
        Err(refusal) => return refusal,
    };
    match read_store_node_as_run(&state.config_trees, tenant_id, store_id, "retention").await {
        Ok(value) => (StatusCode::OK, Json(value)).into_response(),
        Err(response) => response,
    }
}

/// A super-admin publishes how many days a store keeps a synced event, as its `retention` node
/// (ADR-0145). Outside [`EVENT_LOG_DAYS`] is refused, naming the field: a store whose log would
/// age out in a week, or never, is a typo rather than a policy.
pub(super) async fn admin_publish_retention<Cfg, A, C>(
    State(state): State<ConfigChannelsState<Cfg, A, C>>,
    headers: HeaderMap,
    Json(request): Json<PublishRetentionRequest>,
) -> Response
where
    Cfg: ConfigTreeStore + Clone + Send + Sync + 'static,
    A: AdminStore + Clone + Send + Sync + 'static,
    C: ClockSource + Clone + Send + Sync + 'static,
{
    let context = match require_permission(
        &state.admin,
        &state.clock,
        &headers,
        ConsolePermission::PublishConfig,
    )
    .await
    {
        Ok(context) => context,
        Err(denied) => return denied,
    };
    let (tenant_id, store_id) = match parse_ulid_fields([
        ("tenant_id", &request.tenant_id),
        ("store_id", &request.store_id),
    ]) {
        Ok([tenant_id, store_id]) => (TenantId::new(tenant_id), StoreId::new(store_id)),
        Err(refusal) => return refusal,
    };
    if !u16::try_from(request.event_log_days).is_ok_and(|days| EVENT_LOG_DAYS.contains(&days)) {
        return api_error_with_details(
            ErrorStatus::InvalidArgument,
            "event_log_days must be between 30 and 3650",
            &[("event_log_days", "OUT_OF_RANGE")],
        );
    }
    publish_store_settings_node(
        &state,
        &context,
        tenant_id,
        store_id,
        "retention",
        "config.retention.publish",
        serde_json::json!({ "event_log_days": request.event_log_days }),
    )
    .await
}
