// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! `/admin/stores/{store_id}/devices`: list, create and update a store's registered devices.

use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use pos_cloud_api::console::{audit_action, mint_ulid, require_permission};
use pos_cloud_api::response::parse_ulid_fields;
use pos_proto::determinism::ClockSource;
use pos_proto::ids::{DeviceId, StoreId, TenantId};

use super::{
    CreateDeviceRequest, RegistryState, UpdateDeviceRequest, registry_entropy_unavailable,
    registry_error_response,
};
use crate::auth::admin::AdminStore;
use crate::auth::console_rbac::ConsolePermission;
use crate::http::{
    Json, RegistryTenantQuery, entity_status_refusal, if_match, not_found, parse_entity_status,
    version_mismatch, versioned_created, versioned_ok,
};
use crate::registry::{DeviceRecord, EntityStatus, RegistryStore};
use crate::version::UpdateOutcome;

#[utoipa::path(
    get,
    path = "/admin/stores/{store_id}/devices",
    security(("session_cookie" = [])),
    params(
        ("store_id" = String, Path, description = "The store (a 26-character ULID)"),
        ("tenant_id" = String, Query, description = "The store's tenant (a 26-character ULID)"),
    ),
    responses(
        (status = 200, description = "The store's device slots, as an array: `device_id`, \
                                      `tenant_id`, `store_id`, `name`, `kind` and `status`, each \
                                      with its `etag`. These are the slots a box is activated or \
                                      claimed into; their ids are not the ids of the devices a \
                                      store admits itself (`GET /admin/stores/{store_id}/devices/admitted`)"),
        (status = 400, description = "tenant_id or store_id is absent or not a ULID", body = crate::openapi_admin::ErrorResponse),
        (status = 401, description = "No session", body = crate::openapi_admin::ErrorResponse),
        (status = 403, description = "The role lacks console.read", body = crate::openapi_admin::ErrorResponse),
        (status = 503, description = "The registry store failed", body = crate::openapi_admin::ErrorResponse),
    ),
    tag = "box activation",
)]
/// A super-admin lists a store's devices (tenant named on the query).
pub(super) async fn admin_list_devices<Rg, A, C>(
    State(state): State<RegistryState<Rg, A, C>>,
    headers: HeaderMap,
    Path(store_id): Path<String>,
    Query(query): Query<RegistryTenantQuery>,
) -> Response
where
    Rg: RegistryStore + Clone + Send + Sync + 'static,
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
    let (tenant_id, store_id) =
        match parse_ulid_fields([("tenant_id", &query.tenant_id), ("store_id", &store_id)]) {
            Ok([tenant_id, store_id]) => (TenantId::new(tenant_id), StoreId::new(store_id)),
            Err(refusal) => return refusal,
        };
    match state.registry.list_devices(tenant_id, store_id).await {
        Ok(devices) => (StatusCode::OK, Json(devices)).into_response(),
        Err(error) => registry_error_response(&error),
    }
}

#[utoipa::path(
    post,
    path = "/admin/stores/{store_id}/devices",
    security(("session_cookie" = [])),
    params(
        ("store_id" = String, Path, description = "The store (a 26-character ULID)"),
    ),
    request_body(
        description = "`tenant_id`, the slot's `name`, and its `kind`, such as `pos`, `printer`, \
                       `kds` or `tablet`",
        content_type = "application/json",
    ),
    responses(
        (status = 201, description = "Created, active, with the id minted here. The body is the \
                                      record and the ETag carries its version. Audited"),
        (status = 400, description = "tenant_id or store_id is not a ULID", body = crate::openapi_admin::ErrorResponse),
        (status = 401, description = "No session", body = crate::openapi_admin::ErrorResponse),
        (status = 403, description = "The role lacks console.devices.manage", body = crate::openapi_admin::ErrorResponse),
        (status = 503, description = "The registry store or the entropy source is unavailable", body = crate::openapi_admin::ErrorResponse),
    ),
    tag = "box activation",
)]
/// A super-admin creates a device under a store.
pub(super) async fn admin_create_device<Rg, A, C>(
    State(state): State<RegistryState<Rg, A, C>>,
    headers: HeaderMap,
    Path(store_id): Path<String>,
    Json(request): Json<CreateDeviceRequest>,
) -> Response
where
    Rg: RegistryStore + Clone + Send + Sync + 'static,
    A: AdminStore + Clone + Send + Sync + 'static,
    C: ClockSource + Clone + Send + Sync + 'static,
{
    let context = match require_permission(
        &state.admin,
        &state.clock,
        &headers,
        ConsolePermission::ManageDevices,
    )
    .await
    {
        Ok(context) => context,
        Err(denied) => return denied,
    };
    let (tenant_id, store_id) =
        match parse_ulid_fields([("tenant_id", &request.tenant_id), ("store_id", &store_id)]) {
            Ok([tenant_id, store_id]) => (TenantId::new(tenant_id), StoreId::new(store_id)),
            Err(refusal) => return refusal,
        };
    let Some(device_id) =
        mint_ulid(state.clock.now().as_milliseconds_since_epoch()).map(DeviceId::new)
    else {
        return registry_entropy_unavailable();
    };
    let record = DeviceRecord {
        device_id,
        tenant_id,
        store_id,
        name: request.name,
        kind: request.kind,
        status: EntityStatus::Active,
    };
    match state.registry.create_device(&record).await {
        Ok(version) => {
            let after = serde_json::to_value(&record).ok();
            audit_action(
                &state.audit,
                &state.clock,
                &context,
                Some(tenant_id),
                "device.create",
                "device",
                &device_id.to_string(),
                None,
                after,
            )
            .await;
            versioned_created(record, &version)
        }
        Err(error) => registry_error_response(&error),
    }
}

#[utoipa::path(
    patch,
    path = "/admin/stores/{store_id}/devices/{device_id}",
    security(("session_cookie" = [])),
    params(
        ("store_id" = String, Path, description = "The store (a 26-character ULID)"),
        ("device_id" = String, Path, description = "The device slot (a 26-character ULID)"),
        ("If-Match" = String, Header, description = "The ETag the slot was read at (ADR-0094). \
                                                     Required; a wildcard is refused"),
    ),
    request_body(
        description = "`tenant_id`, and the slot's `name`, `kind` and `status` (`active` or \
                       `archived`) as they should now stand",
        content_type = "application/json",
    ),
    responses(
        (status = 200, description = "Updated. The body is the record and the ETag carries its new \
                                      version. Audited"),
        (status = 400, description = "An id is not a ULID, status is not `active` or `archived`, or \
                                      If-Match is absent, malformed, or a wildcard", body = crate::openapi_admin::ErrorResponse),
        (status = 401, description = "No session", body = crate::openapi_admin::ErrorResponse),
        (status = 403, description = "The role lacks console.devices.manage", body = crate::openapi_admin::ErrorResponse),
        (status = 404, description = "The store has no device slot with that id", body = crate::openapi_admin::ErrorResponse),
        (status = 412, description = "The slot changed since you read it: re-read it and try again", body = crate::openapi_admin::ErrorResponse),
        (status = 503, description = "The registry store failed", body = crate::openapi_admin::ErrorResponse),
    ),
    tag = "box activation",
)]
/// A super-admin renames a device, sets its kind, and/or sets its status.
pub(super) async fn admin_update_device<Rg, A, C>(
    State(state): State<RegistryState<Rg, A, C>>,
    headers: HeaderMap,
    Path((store_id, device_id)): Path<(String, String)>,
    Json(request): Json<UpdateDeviceRequest>,
) -> Response
where
    Rg: RegistryStore + Clone + Send + Sync + 'static,
    A: AdminStore + Clone + Send + Sync + 'static,
    C: ClockSource + Clone + Send + Sync + 'static,
{
    let context = match require_permission(
        &state.admin,
        &state.clock,
        &headers,
        ConsolePermission::ManageDevices,
    )
    .await
    {
        Ok(context) => context,
        Err(denied) => return denied,
    };
    let (tenant_id, store_id, device_id) = match parse_ulid_fields([
        ("tenant_id", &request.tenant_id),
        ("store_id", &store_id),
        ("device_id", &device_id),
    ]) {
        Ok([tenant_id, store_id, device_id]) => (
            TenantId::new(tenant_id),
            StoreId::new(store_id),
            DeviceId::new(device_id),
        ),
        Err(refusal) => return refusal,
    };
    let Some(status) = parse_entity_status(&request.status) else {
        return entity_status_refusal();
    };
    let record = DeviceRecord {
        device_id,
        tenant_id,
        store_id,
        name: request.name,
        kind: request.kind,
        status,
    };
    let expected = match if_match(&headers) {
        Ok(expected) => expected,
        Err(refusal) => return refusal,
    };
    match state.registry.update_device(&record, &expected).await {
        Ok(UpdateOutcome::Updated(version)) => {
            let after = serde_json::to_value(&record).ok();
            audit_action(
                &state.audit,
                &state.clock,
                &context,
                Some(tenant_id),
                "device.update",
                "device",
                &device_id.to_string(),
                None,
                after,
            )
            .await;
            versioned_ok(record, &version)
        }
        Ok(UpdateOutcome::VersionMismatch) => version_mismatch(),
        Ok(UpdateOutcome::NotFound) => not_found("device"),
        Err(error) => registry_error_response(&error),
    }
}
