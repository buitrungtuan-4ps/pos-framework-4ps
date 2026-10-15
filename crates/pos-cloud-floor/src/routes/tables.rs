// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! `/admin/floor/tables`: list, read, create and update a store's tables.

use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use pos_cloud_api::console::{audit_action, mint_ulid, require_permission};
use pos_cloud_api::response::{api_error_with_details, parse_ulid_fields};
use pos_proto::ErrorStatus;
use pos_proto::determinism::ClockSource;
use pos_proto::ids::{AreaId, StoreId, TableId, TenantId};

use super::requests::{
    CreateTableRequest, FloorListQuery, FloorTenantQuery, UpdateTableRequest, grid_position,
};
use super::{
    FloorState, floor_entropy_unavailable, floor_error_response, floor_tenant, floor_tenant_store,
};
use crate::auth::admin::AdminStore;
use crate::auth::console_rbac::ConsolePermission;
use crate::catalog::CatalogStore;
use crate::floorplan::{NewTable, TableStore, TableUpdate};
use crate::registry::EntityStatus;
use crate::version::UpdateOutcome;
use axum::Json;
use pos_cloud_api::conditional::{if_match, version_mismatch, versioned_ok, with_etag};
use pos_cloud_api::refusal::not_found;
use pos_cloud_config::route::{entity_status_refusal, parse_entity_status};

pub(super) async fn admin_list_tables<F, A, C, Cat>(
    State(state): State<FloorState<F, A, C, Cat>>,
    headers: HeaderMap,
    Query(query): Query<FloorListQuery>,
) -> Response
where
    F: TableStore + Clone + Send + Sync + 'static,
    A: AdminStore + Clone + Send + Sync + 'static,
    C: ClockSource + Clone + Send + Sync + 'static,
    Cat: CatalogStore + Clone + Send + Sync + 'static,
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
    let (tenant_id, store_id) = match floor_tenant_store(&query) {
        Ok(scope) => scope,
        Err(response) => return response,
    };
    match TableStore::list(&state.floor, tenant_id, store_id).await {
        Ok(tables) => (StatusCode::OK, Json(tables)).into_response(),
        Err(error) => floor_error_response(&error),
    }
}

pub(super) async fn admin_get_table<F, A, C, Cat>(
    State(state): State<FloorState<F, A, C, Cat>>,
    headers: HeaderMap,
    Path(table_id): Path<String>,
    Query(query): Query<FloorTenantQuery>,
) -> Response
where
    F: TableStore + Clone + Send + Sync + 'static,
    A: AdminStore + Clone + Send + Sync + 'static,
    C: ClockSource + Clone + Send + Sync + 'static,
    Cat: CatalogStore + Clone + Send + Sync + 'static,
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
    let tenant_id = match floor_tenant(&query.tenant_id) {
        Ok(id) => id,
        Err(response) => return response,
    };
    let table_id = match parse_ulid_fields([("table_id", &table_id)]) {
        Ok([table_id]) => TableId::new(table_id),
        Err(refusal) => return refusal,
    };
    match TableStore::get(&state.floor, tenant_id, table_id).await {
        Ok(Some(table)) => versioned_ok(table.record, &table.etag),
        Ok(None) => not_found("table"),
        Err(error) => floor_error_response(&error),
    }
}

pub(super) async fn admin_create_table<F, A, C, Cat>(
    State(state): State<FloorState<F, A, C, Cat>>,
    headers: HeaderMap,
    Json(request): Json<CreateTableRequest>,
) -> Response
where
    F: TableStore + Clone + Send + Sync + 'static,
    A: AdminStore + Clone + Send + Sync + 'static,
    C: ClockSource + Clone + Send + Sync + 'static,
    Cat: CatalogStore + Clone + Send + Sync + 'static,
{
    let context = match require_permission(
        &state.admin,
        &state.clock,
        &headers,
        ConsolePermission::ManageFloor,
    )
    .await
    {
        Ok(context) => context,
        Err(denied) => return denied,
    };
    let (tenant_id, store_id, area_id) = match parse_ulid_fields([
        ("tenant_id", &request.tenant_id),
        ("store_id", &request.store_id),
        ("area_id", &request.area_id),
    ]) {
        Ok([tenant_id, store_id, area_id]) => (
            TenantId::new(tenant_id),
            StoreId::new(store_id),
            AreaId::new(area_id),
        ),
        Err(refusal) => return refusal,
    };
    if request.name.trim().is_empty() {
        return api_error_with_details(
            ErrorStatus::InvalidArgument,
            "name is required",
            &[("name", "REQUIRED")],
        );
    }
    let Some(table_id) =
        mint_ulid(state.clock.now().as_milliseconds_since_epoch()).map(TableId::new)
    else {
        return floor_entropy_unavailable();
    };
    let new_table = NewTable {
        table_id,
        tenant_id,
        store_id,
        area_id,
        label: request.name.clone(),
        seats: request.seats,
        position: grid_position(request.grid_column, request.grid_row),
    };
    match TableStore::create(&state.floor, &new_table).await {
        Ok(version) => {
            let after = serde_json::json!({
                "id": table_id.to_string(),
                "area_id": area_id.to_string(),
                "label": request.name,
                "seats": request.seats,
                "status": EntityStatus::Active.as_str(),
            });
            audit_action(
                &state.audit,
                &state.clock,
                &context,
                Some(tenant_id),
                "floor.table.create",
                "floor_table",
                &table_id.to_string(),
                None,
                Some(after),
            )
            .await;
            with_etag(
                (
                    StatusCode::CREATED,
                    Json(serde_json::json!({ "id": table_id.to_string() })),
                )
                    .into_response(),
                &version,
            )
        }
        Err(error) => floor_error_response(&error),
    }
}

pub(super) async fn admin_update_table<F, A, C, Cat>(
    State(state): State<FloorState<F, A, C, Cat>>,
    headers: HeaderMap,
    Path(table_id): Path<String>,
    Json(request): Json<UpdateTableRequest>,
) -> Response
where
    F: TableStore + Clone + Send + Sync + 'static,
    A: AdminStore + Clone + Send + Sync + 'static,
    C: ClockSource + Clone + Send + Sync + 'static,
    Cat: CatalogStore + Clone + Send + Sync + 'static,
{
    let context = match require_permission(
        &state.admin,
        &state.clock,
        &headers,
        ConsolePermission::ManageFloor,
    )
    .await
    {
        Ok(context) => context,
        Err(denied) => return denied,
    };
    let (tenant_id, table_id, area_id) = match parse_ulid_fields([
        ("tenant_id", &request.tenant_id),
        ("table_id", &table_id),
        ("area_id", &request.area_id),
    ]) {
        Ok([tenant_id, table_id, area_id]) => (
            TenantId::new(tenant_id),
            TableId::new(table_id),
            AreaId::new(area_id),
        ),
        Err(refusal) => return refusal,
    };
    if request.name.trim().is_empty() {
        return api_error_with_details(
            ErrorStatus::InvalidArgument,
            "name is required",
            &[("name", "REQUIRED")],
        );
    }
    let Some(status) = parse_entity_status(&request.status) else {
        return entity_status_refusal();
    };
    let update = TableUpdate {
        table_id,
        tenant_id,
        area_id,
        label: request.name.clone(),
        seats: request.seats,
        position: grid_position(request.grid_column, request.grid_row),
        status,
    };
    let expected = match if_match(&headers) {
        Ok(expected) => expected,
        Err(refusal) => return refusal,
    };
    match TableStore::update(&state.floor, &update, &expected).await {
        Ok(UpdateOutcome::Updated(version)) => {
            let after = serde_json::json!({
                "id": table_id.to_string(),
                "area_id": area_id.to_string(),
                "label": request.name,
                "seats": request.seats,
                "status": status.as_str(),
            });
            audit_action(
                &state.audit,
                &state.clock,
                &context,
                Some(tenant_id),
                "floor.table.update",
                "floor_table",
                &table_id.to_string(),
                None,
                Some(after),
            )
            .await;
            with_etag(StatusCode::NO_CONTENT.into_response(), &version)
        }
        Ok(UpdateOutcome::VersionMismatch) => version_mismatch(),
        Ok(UpdateOutcome::NotFound) => not_found("table"),
        Err(error) => floor_error_response(&error),
    }
}
