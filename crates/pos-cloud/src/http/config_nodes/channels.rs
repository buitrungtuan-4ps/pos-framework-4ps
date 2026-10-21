// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! `/admin/config/channels`, `/admin/config/tender` and `/admin/config/origins`: read and publish.

use axum::extract::{Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use pos_cloud_api::console::require_permission;
use pos_cloud_api::response::parse_ulid_fields;
use pos_proto::determinism::ClockSource;
use pos_proto::ids::{StoreId, TenantId};

use super::compilers::{channels_node, origins_node, tender_node};
use super::{
    ConfigChannelsState, ConfigNodeQuery, PublishChannelsRequest, PublishOriginsRequest,
    PublishTenderRequest, publish_store_settings_node,
};
use crate::auth::admin::AdminStore;
use crate::auth::console_rbac::ConsolePermission;
use crate::config_tree::ConfigTreeStore;
use crate::http::{Json, read_store_node_as_run};

#[utoipa::path(
    get,
    path = "/admin/config/channels",
    extensions(("x-permission" = json!("console.data.read"))),
    security(("session_cookie" = [])),
    params(
        ("tenant_id" = String, Query, description = "The store's tenant (a 26-character ULID)"),
        ("store_id" = String, Query, description = "The store (a 26-character ULID)"),
    ),
    responses(
        (status = 200, description = "The store's `channels` node as the store runs it — the sales channels it takes orders on — \
                                      composed from all four layers, or `null` when it has none"),
        (status = 400, description = "tenant_id or store_id is absent or not a ULID", body = crate::openapi_admin::ErrorResponse),
        (status = 401, description = "No session", body = crate::openapi_admin::ErrorResponse),
        (status = 403, description = "The role lacks console.data.read", body = crate::openapi_admin::ErrorResponse),
        (status = 503, description = "The configuration store is unreachable", body = crate::openapi_admin::ErrorResponse),
    ),
    tag = "config nodes",
)]
/// A super-admin reads a store's current `channels` node (the enabled sales channels), or `null`.
pub(super) async fn admin_read_channels<Cfg, A, C>(
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
    match read_store_node_as_run(&state.config_trees, tenant_id, store_id, "channels").await {
        Ok(value) => (StatusCode::OK, Json(value)).into_response(),
        Err(response) => response,
    }
}

#[utoipa::path(
    put,
    path = "/admin/config/channels",
    extensions(("x-permission" = json!("console.config.publish"))),
    security(("session_cookie" = [])),
    request_body(
        description = "`tenant_id` and `store_id`, then `enabled`, the sales-channel tokens the store takes orders on (ADR-0080)",
        content_type = "application/json",
    ),
    responses(
        (status = 200, description = "Published as the store's `channels` node on its Store layer. \
                                      The body carries `config_version_id`, which the store pulls on \
                                      its next sync. Audited with the node's value"),
        (status = 400, description = "tenant_id or store_id is not a ULID, a token is not a sales channel, or a value needs \
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
/// A super-admin publishes a store's enabled sales channels as its `channels` node.
pub(super) async fn admin_publish_channels<Cfg, A, C>(
    State(state): State<ConfigChannelsState<Cfg, A, C>>,
    headers: HeaderMap,
    Json(request): Json<PublishChannelsRequest>,
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
    let value = match channels_node(&request.enabled) {
        Ok(value) => value,
        Err(refusal) => return refusal,
    };
    publish_store_settings_node(
        &state,
        &context,
        tenant_id,
        store_id,
        "channels",
        "config.channels.publish",
        value,
    )
    .await
}

#[utoipa::path(
    get,
    path = "/admin/config/tender",
    extensions(("x-permission" = json!("console.data.read"))),
    security(("session_cookie" = [])),
    params(
        ("tenant_id" = String, Query, description = "The store's tenant (a 26-character ULID)"),
        ("store_id" = String, Query, description = "The store (a 26-character ULID)"),
    ),
    responses(
        (status = 200, description = "The store's `tender` node as the store runs it — the payment methods it accepts — \
                                      composed from all four layers, or `null` when it has none"),
        (status = 400, description = "tenant_id or store_id is absent or not a ULID", body = crate::openapi_admin::ErrorResponse),
        (status = 401, description = "No session", body = crate::openapi_admin::ErrorResponse),
        (status = 403, description = "The role lacks console.data.read", body = crate::openapi_admin::ErrorResponse),
        (status = 503, description = "The configuration store is unreachable", body = crate::openapi_admin::ErrorResponse),
    ),
    tag = "config nodes",
)]
/// A super-admin reads a store's current `tender` node (the accepted payment methods), or `null`.
pub(super) async fn admin_read_tender<Cfg, A, C>(
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
    match read_store_node_as_run(&state.config_trees, tenant_id, store_id, "tender").await {
        Ok(value) => (StatusCode::OK, Json(value)).into_response(),
        Err(response) => response,
    }
}

#[utoipa::path(
    put,
    path = "/admin/config/tender",
    extensions(("x-permission" = json!("console.config.publish"))),
    security(("session_cookie" = [])),
    request_body(
        description = "`tenant_id` and `store_id`, then `accepted`, the payment-method tokens the store takes (ADR-0080)",
        content_type = "application/json",
    ),
    responses(
        (status = 200, description = "Published as the store's `tender` node on its Store layer. \
                                      The body carries `config_version_id`, which the store pulls on \
                                      its next sync. Audited with the node's value"),
        (status = 400, description = "tenant_id or store_id is not a ULID, a token is not a payment method, or a value needs \
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
/// A super-admin publishes a store's accepted payment methods as its `tender` node.
pub(super) async fn admin_publish_tender<Cfg, A, C>(
    State(state): State<ConfigChannelsState<Cfg, A, C>>,
    headers: HeaderMap,
    Json(request): Json<PublishTenderRequest>,
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
    let value = match tender_node(&request.accepted) {
        Ok(value) => value,
        Err(refusal) => return refusal,
    };
    publish_store_settings_node(
        &state,
        &context,
        tenant_id,
        store_id,
        "tender",
        "config.tender.publish",
        value,
    )
    .await
}

#[utoipa::path(
    get,
    path = "/admin/config/origins",
    extensions(("x-permission" = json!("console.data.read"))),
    security(("session_cookie" = [])),
    params(
        ("tenant_id" = String, Query, description = "The store's tenant (a 26-character ULID)"),
        ("store_id" = String, Query, description = "The store (a 26-character ULID)"),
    ),
    responses(
        (status = 200, description = "The store's `origins` node as the store runs it — the origins its edge answers besides its own (ADR-0111) — \
                                      composed from all four layers, or `null` when it has none"),
        (status = 400, description = "tenant_id or store_id is absent or not a ULID", body = crate::openapi_admin::ErrorResponse),
        (status = 401, description = "No session", body = crate::openapi_admin::ErrorResponse),
        (status = 403, description = "The role lacks console.data.read", body = crate::openapi_admin::ErrorResponse),
        (status = 503, description = "The configuration store is unreachable", body = crate::openapi_admin::ErrorResponse),
    ),
    tag = "config nodes",
)]
/// A super-admin reads a store's current `origins` node (the origins its edge answers), or `null`.
pub(super) async fn admin_read_origins<Cfg, A, C>(
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
    match read_store_node_as_run(&state.config_trees, tenant_id, store_id, "origins").await {
        Ok(value) => (StatusCode::OK, Json(value)).into_response(),
        Err(response) => response,
    }
}

#[utoipa::path(
    put,
    path = "/admin/config/origins",
    extensions(("x-permission" = json!("console.config.publish"))),
    security(("session_cookie" = [])),
    request_body(
        description = "`tenant_id` and `store_id`, then `allowed`, the origins the store's edge answers besides the one serving it, each checked by the rule the edge applies (ADR-0111). An empty list withdraws every second origin; the store's own UI is never affected",
        content_type = "application/json",
    ),
    responses(
        (status = 200, description = "Published as the store's `origins` node on its Store layer. \
                                      The body carries `config_version_id`, which the store pulls on \
                                      its next sync. Audited with the node's value"),
        (status = 400, description = "tenant_id or store_id is not a ULID, an origin is refused by that rule, or a value needs \
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
/// A super-admin publishes the origins a store's edge answers as its `origins` node
/// ([ADR-0111](../../../docs/adr/0111-a-second-origin-may-address-the-edge.md)).
///
/// Every entry is validated here, against the very rule the edge applies
/// ([`pos_proto::origins::validate_origins`]), so a typo is a `400` the operator sees rather than a
/// node the edge refuses whole and silently keeps the previous list for. That refusal is deliberately
/// symmetric: authoring rejects what applying would reject.
///
/// An **empty** `allowed` is accepted and is not the same as never publishing: it is a person
/// deliberately withdrawing every second origin. A store's own UI is unaffected either way — the edge
/// decides same-origin against the request's `Host` and never against this list.
pub(super) async fn admin_publish_origins<Cfg, A, C>(
    State(state): State<ConfigChannelsState<Cfg, A, C>>,
    headers: HeaderMap,
    Json(request): Json<PublishOriginsRequest>,
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
    let value = match origins_node(&request.allowed) {
        Ok(value) => value,
        Err(refusal) => return refusal,
    };
    publish_store_settings_node(
        &state,
        &context,
        tenant_id,
        store_id,
        "origins",
        "config.origins.publish",
        value,
    )
    .await
}
