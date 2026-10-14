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
