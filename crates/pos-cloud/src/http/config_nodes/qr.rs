// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! `/admin/config/qr`: a store's QR ordering guardrails and business hours, read and publish.

use axum::extract::{Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use pos_cloud_api::console::require_permission;
use pos_cloud_api::response::parse_ulid_fields;
use pos_proto::determinism::ClockSource;
use pos_proto::ids::{StoreId, TenantId};
use serde::Deserialize;

use super::compilers::{QrGuardrailFields, qr_guardrail_node};
use super::{ConfigChannelsState, ConfigNodeQuery, publish_store_settings_node};
use crate::auth::admin::AdminStore;
use crate::auth::console_rbac::ConsolePermission;
use crate::config_tree::ConfigTreeStore;
use crate::http::{Json, read_store_node_as_run};

/// A `qr.business_hours` window in a `PUT /admin/config/qr` body.
#[derive(Debug, Clone, Deserialize, utoipa::ToSchema)]
pub(in crate::http) struct QrBusinessHoursRequest {
    pub(in crate::http) open_hour: u8,
    pub(in crate::http) close_hour: u8,
    #[serde(default)]
    pub(in crate::http) tz_offset_minutes: i64,
}

/// A `PUT /admin/config/qr` body: the store and the QR guardrail settings (P11b/ADR-0057). Composed
/// into the `qr` node the cloud's QR intake reads (`qr_http::qr_config_for`) and the edge reads for its
/// staff-confirmation decision.
#[derive(Debug, Clone, Deserialize, utoipa::ToSchema)]
pub(super) struct PublishQrRequest {
    tenant_id: String,
    store_id: String,
    /// QR ordering's switch, as [`QrGuardrailFields::enabled`] takes it: optional, and written as
    /// the switch when sent.
    #[serde(default)]
    enabled: Option<bool>,
    staff_confirmation_required: bool,
    per_table_limit: u32,
    rate_window_secs: u64,
    #[serde(default)]
    business_hours: Option<QrBusinessHoursRequest>,
}

#[utoipa::path(
    get,
    path = "/admin/config/qr",
    extensions(("x-permission" = json!("console.data.read"))),
    security(("session_cookie" = [])),
    params(
        ("tenant_id" = String, Query, description = "The store's tenant (a 26-character ULID)"),
        ("store_id" = String, Query, description = "The store (a 26-character ULID)"),
    ),
    responses(
        (status = 200, description = "The store's `qr` node as the store runs it — whether guests may order by QR and the guardrails on it — \
                                      composed from all four layers, or `null` when it has none"),
        (status = 400, description = "tenant_id or store_id is absent or not a ULID", body = crate::openapi_admin::ErrorResponse),
        (status = 401, description = "No session", body = crate::openapi_admin::ErrorResponse),
        (status = 403, description = "The role lacks console.data.read", body = crate::openapi_admin::ErrorResponse),
        (status = 503, description = "The configuration store is unreachable", body = crate::openapi_admin::ErrorResponse),
    ),
    tag = "config nodes",
)]
/// A super-admin reads a store's current `qr` guardrail node, or `null`.
pub(super) async fn admin_read_qr<Cfg, A, C>(
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
    match read_store_node_as_run(&state.config_trees, tenant_id, store_id, "qr").await {
        Ok(value) => (StatusCode::OK, Json(value)).into_response(),
        Err(response) => response,
    }
}

#[utoipa::path(
    put,
    path = "/admin/config/qr",
    extensions(("x-permission" = json!("console.config.publish"))),
    security(("session_cookie" = [])),
    request_body(
        content = PublishQrRequest, description = "`tenant_id` and `store_id`, then the QR guardrails (ADR-0057): optionally `enabled`, `staff_confirmation_required`, `per_table_limit`, `rate_window_secs`, and optionally `business_hours`, an opening and closing hour",
        content_type = "application/json",
    ),
    responses(
        (status = 200, body = pos_cloud_config::route::PublishedConfig, description = "Published as the store's `qr` node on its Store layer. \
                                      The body carries `config_version_id`, which the store pulls on \
                                      its next sync. Audited with the node's value"),
        (status = 400, description = "tenant_id or store_id is not a ULID, an hour is out of range, or a value needs \
                                      a recorded confirmation first (ADR-0169)", body = crate::openapi_admin::ErrorResponse),
        (status = 401, description = "No session", body = crate::openapi_admin::ErrorResponse),
        (status = 403, description = "The role lacks console.config.publish", body = crate::openapi_admin::ErrorResponse),
        (status = 412, description = "Every attempt to write the store's configuration lost a race \
                                      with another publish: try again", body = crate::openapi_admin::ErrorResponse),
        (status = 422, description = "The store's configuration would not validate with the node", body = crate::openapi_admin::ErrorResponse),
        (status = 503, description = "The configuration store is unreachable", body = crate::openapi_admin::ErrorResponse),
    ),
    tag = "config nodes",
)]
/// A super-admin publishes a store's QR guardrail settings as its `qr` node.
pub(super) async fn admin_publish_qr<Cfg, A, C>(
    State(state): State<ConfigChannelsState<Cfg, A, C>>,
    headers: HeaderMap,
    Json(request): Json<PublishQrRequest>,
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
    let node = match qr_guardrail_node(&QrGuardrailFields {
        enabled: request.enabled,
        staff_confirmation_required: request.staff_confirmation_required,
        per_table_limit: request.per_table_limit,
        rate_window_secs: request.rate_window_secs,
        business_hours: request.business_hours,
    }) {
        Ok(node) => node,
        Err(refusal) => return refusal,
    };
    publish_store_settings_node(
        &state,
        &context,
        tenant_id,
        store_id,
        "qr",
        "config.qr.publish",
        node,
    )
    .await
}
