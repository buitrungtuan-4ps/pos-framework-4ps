// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! `/admin/config/vendors`: the vendor policies a store applies, read and publish.

use axum::extract::{Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use pos_cloud_api::console::require_permission;
use pos_cloud_api::response::parse_ulid_fields;
use pos_proto::channels::PublishedVendorPolicy;
use pos_proto::determinism::ClockSource;
use pos_proto::ids::{StoreId, TenantId};
use serde::Deserialize;

use super::compilers::vendor_policies_node;
use super::{ConfigChannelsState, ConfigNodeQuery, publish_store_settings_node};
use crate::auth::admin::AdminStore;
use crate::auth::console_rbac::ConsolePermission;
use crate::config_tree::ConfigTreeStore;
use crate::http::{Json, read_store_node_as_run};

/// A `PUT /admin/config/vendors` body: the store and its per-marketplace policies.
#[derive(Debug, Clone, Deserialize)]
pub(super) struct PublishVendorsRequest {
    tenant_id: String,
    store_id: String,
    #[serde(default)]
    policies: Vec<PublishedVendorPolicy>,
}

#[utoipa::path(
    get,
    path = "/admin/config/vendors",
    extensions(("x-permission" = json!("console.data.read"))),
    security(("session_cookie" = [])),
    params(
        ("tenant_id" = String, Query, description = "The store's tenant (a 26-character ULID)"),
        ("store_id" = String, Query, description = "The store (a 26-character ULID)"),
    ),
    responses(
        (status = 200, description = "The store's `vendors` node as the store runs it — its policy for each delivery marketplace — \
                                      composed from all four layers, or `null` when it has none"),
        (status = 400, description = "tenant_id or store_id is absent or not a ULID", body = crate::openapi_admin::ErrorResponse),
        (status = 401, description = "No session", body = crate::openapi_admin::ErrorResponse),
        (status = 403, description = "The role lacks console.data.read", body = crate::openapi_admin::ErrorResponse),
        (status = 503, description = "The configuration store is unreachable", body = crate::openapi_admin::ErrorResponse),
    ),
    tag = "config nodes",
)]
/// A super-admin reads a store's current `vendors` policy node, or `null`.
pub(super) async fn admin_read_vendors<Cfg, A, C>(
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
    match read_store_node_as_run(&state.config_trees, tenant_id, store_id, "vendors").await {
        Ok(value) => (StatusCode::OK, Json(value)).into_response(),
        Err(response) => response,
    }
}

#[utoipa::path(
    put,
    path = "/admin/config/vendors",
    extensions(("x-permission" = json!("console.config.publish"))),
    security(("session_cookie" = [])),
    request_body(
        description = "`tenant_id` and `store_id`, then `policies`, one per delivery marketplace: its name, whether the store takes its orders, the store's availability to it, and the preparation time it quotes while the store is busy",
        content_type = "application/json",
    ),
    responses(
        (status = 200, description = "Published as the store's `vendors` node on its Store layer. \
                                      The body carries `config_version_id`, which the store pulls on \
                                      its next sync. Audited with the node's value"),
        (status = 400, description = "tenant_id or store_id is not a ULID, an availability is not a recognised value, or a value needs \
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
/// A super-admin publishes a store's per-marketplace policies as its `vendors` node. Each policy's
/// availability must be a recognised value; the live loop that pushes it to a marketplace is deferred.
pub(super) async fn admin_publish_vendors<Cfg, A, C>(
    State(state): State<ConfigChannelsState<Cfg, A, C>>,
    headers: HeaderMap,
    Json(request): Json<PublishVendorsRequest>,
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
    let value = match vendor_policies_node(request.policies) {
        Ok(value) => value,
        Err(refusal) => return refusal,
    };
    publish_store_settings_node(
        &state,
        &context,
        tenant_id,
        store_id,
        "vendors",
        "config.vendors.publish",
        value,
    )
    .await
}
