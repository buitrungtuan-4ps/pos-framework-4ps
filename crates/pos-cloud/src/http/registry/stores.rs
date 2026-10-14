// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! `/admin/stores`: list, create and update a tenant's stores.

use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use pos_cloud_api::console::{audit_action, mint_ulid, require_permission};
use pos_cloud_api::response::{parse_ulid_fields, ulid_refusal};
use pos_proto::determinism::ClockSource;
use pos_proto::ids::{StoreId, TenantId};

use super::{
    CreateStoreRequest, RegistryState, UpdateStoreRequest, registry_entropy_unavailable,
    registry_error_response,
};
use crate::auth::admin::AdminStore;
use crate::auth::console_rbac::ConsolePermission;
use crate::http::{
    Json, RegistryTenantQuery, entity_status_refusal, if_match, not_found, parse_entity_status,
    parse_optional_ulid, version_mismatch, versioned_created, versioned_ok,
};
use crate::registry::{BrandId, EntityStatus, RegistryStore, StoreRecord};
use crate::version::UpdateOutcome;

/// A super-admin lists a tenant's stores.
pub(super) async fn admin_list_stores<Rg, A, C>(
    State(state): State<RegistryState<Rg, A, C>>,
    headers: HeaderMap,
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
    let tenant_id = match parse_ulid_fields([("tenant_id", &query.tenant_id)]) {
        Ok([tenant_id]) => TenantId::new(tenant_id),
        Err(refusal) => return refusal,
    };
    match state.registry.list_stores(tenant_id).await {
        Ok(stores) => (StatusCode::OK, Json(stores)).into_response(),
        Err(error) => registry_error_response(&error),
    }
}

/// A super-admin creates a store under a tenant, with an optional brand.
pub(super) async fn admin_create_store<Rg, A, C>(
    State(state): State<RegistryState<Rg, A, C>>,
    headers: HeaderMap,
    Json(request): Json<CreateStoreRequest>,
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
        ConsolePermission::ManageStores,
    )
    .await
    {
        Ok(context) => context,
        Err(denied) => return denied,
    };
    let tenant_id = match parse_ulid_fields([("tenant_id", &request.tenant_id)]) {
        Ok([tenant_id]) => TenantId::new(tenant_id),
        Err(refusal) => return refusal,
    };
    let Ok(brand_id) = parse_optional_ulid(request.brand_id.as_deref(), BrandId::new) else {
        return ulid_refusal(&["brand_id"]);
    };
    let Some(store_id) =
        mint_ulid(state.clock.now().as_milliseconds_since_epoch()).map(StoreId::new)
    else {
        return registry_entropy_unavailable();
    };
    let record = StoreRecord {
        store_id,
        tenant_id,
        brand_id,
        name: request.name,
        status: EntityStatus::Active,
    };
    match state.registry.create_store(&record).await {
        Ok(version) => {
            let after = serde_json::to_value(&record).ok();
            audit_action(
                &state.audit,
                &state.clock,
                &context,
                Some(tenant_id),
                "store.create",
                "store",
                &store_id.to_string(),
                None,
                after,
            )
            .await;
            versioned_created(record, &version)
        }
        Err(error) => registry_error_response(&error),
    }
}

/// A super-admin renames a store, (re)assigns or clears its brand, and/or sets its status.
pub(super) async fn admin_update_store<Rg, A, C>(
    State(state): State<RegistryState<Rg, A, C>>,
    headers: HeaderMap,
    Path(store_id): Path<String>,
    Json(request): Json<UpdateStoreRequest>,
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
        ConsolePermission::ManageStores,
    )
    .await
    {
        Ok(context) => context,
        Err(denied) => return denied,
    };
    let (store_id, tenant_id) =
        match parse_ulid_fields([("store_id", &store_id), ("tenant_id", &request.tenant_id)]) {
            Ok([store_id, tenant_id]) => (StoreId::new(store_id), TenantId::new(tenant_id)),
            Err(refusal) => return refusal,
        };
    let Ok(brand_id) = parse_optional_ulid(request.brand_id.as_deref(), BrandId::new) else {
        return ulid_refusal(&["brand_id"]);
    };
    let Some(status) = parse_entity_status(&request.status) else {
        return entity_status_refusal();
    };
    let record = StoreRecord {
        store_id,
        tenant_id,
        brand_id,
        name: request.name,
        status,
    };
    let expected = match if_match(&headers) {
        Ok(expected) => expected,
        Err(refusal) => return refusal,
    };
    match state.registry.update_store(&record, &expected).await {
        Ok(UpdateOutcome::Updated(version)) => {
            let after = serde_json::to_value(&record).ok();
            audit_action(
                &state.audit,
                &state.clock,
                &context,
                Some(tenant_id),
                "store.update",
                "store",
                &store_id.to_string(),
                None,
                after,
            )
            .await;
            versioned_ok(record, &version)
        }
        Ok(UpdateOutcome::VersionMismatch) => version_mismatch(),
        Ok(UpdateOutcome::NotFound) => not_found("store"),
        Err(error) => registry_error_response(&error),
    }
}
