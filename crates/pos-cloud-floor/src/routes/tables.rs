// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! `/admin/floor/tables`: list, read, create and update a store's tables.

use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use pos_cloud_api::console::{audit_action, mint_ulid, require_permission};
use pos_cloud_api::response::{api_error_with_details, parse_ulid_fields};
use pos_cloud_api::version::Versioned;
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
use crate::floorplan::{NewTable, Table, TableStore, TableUpdate};
use crate::registry::EntityStatus;
use crate::version::UpdateOutcome;
use axum::Json;
use pos_cloud_api::conditional::{if_match, version_mismatch, versioned_ok, with_etag};
use pos_cloud_api::refusal::not_found;
use pos_cloud_config::route::{entity_status_refusal, parse_entity_status};

#[utoipa::path(
    get,
    path = "/admin/floor/tables",
    extensions(("x-permission" = json!("console.data.read"))),
    security(("session_cookie" = [])),
    params(
        ("tenant_id" = String, Query, description = "The store's tenant (a 26-character ULID)"),
        ("store_id" = String, Query, description = "The store (a 26-character ULID)"),
    ),
    responses(
        (status = 200, body = [Versioned<Table>], description = "The store's tables, active and archived, as an array, each with its `etag`"),
        (status = 400, description = "tenant_id or store_id is absent or not a ULID", body = pos_cloud_api::error_schema::ErrorResponse),
        (status = 401, description = "No session", body = pos_cloud_api::error_schema::ErrorResponse),
        (status = 403, description = "The role lacks console.data.read", body = pos_cloud_api::error_schema::ErrorResponse),
        (status = 503, description = "The floor store is unreachable", body = pos_cloud_api::error_schema::ErrorResponse),
    ),
    tag = "floor",
)]
/// A super-admin lists a store's floor tables.
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

#[utoipa::path(
    get,
    path = "/admin/floor/tables/{table_id}",
    extensions(("x-permission" = json!("console.data.read"))),
    security(("session_cookie" = [])),
    params(
        ("table_id" = String, Path, description = "The table (a 26-character ULID)"),
        ("tenant_id" = String, Query, description = "The tenant the record belongs to (a 26-character ULID)"),
    ),
    responses(
        (status = 200, body = Versioned<Table>, description = "The table. The ETag carries the version it was read at"),
        (status = 400, description = "table_id or tenant_id is absent or not a ULID", body = pos_cloud_api::error_schema::ErrorResponse),
        (status = 401, description = "No session", body = pos_cloud_api::error_schema::ErrorResponse),
        (status = 403, description = "The role lacks console.data.read", body = pos_cloud_api::error_schema::ErrorResponse),
        (status = 404, description = "The tenant has no table with that id", body = pos_cloud_api::error_schema::ErrorResponse),
        (status = 503, description = "The floor store is unreachable", body = pos_cloud_api::error_schema::ErrorResponse),
    ),
    tag = "floor",
)]
/// A super-admin reads one floor table, with the version it was read at.
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

#[utoipa::path(
    post,
    path = "/admin/floor/tables",
    extensions(("x-permission" = json!("console.floor.manage"))),
    security(("session_cookie" = [])),
    request_body(
        content = CreateTableRequest, description = "`tenant_id`, `store_id` and `area_id`; the table's `name`, the label a host reads, which may \
                       not be blank; optionally `seats`, `0` or absent when unspecified; and optionally \
                       `grid_column` and `grid_row`, its place in the floor editor, which count only \
                       when both are given",
        content_type = "application/json",
    ),
    responses(
        (status = 201, description = "Created, active, with the id minted here. The body carries `id`, and the ETag carries \
                                      its version. Audited"),
        (status = 400, description = "tenant_id, store_id or area_id is absent or not a ULID, or name is blank", body = pos_cloud_api::error_schema::ErrorResponse),
        (status = 401, description = "No session", body = pos_cloud_api::error_schema::ErrorResponse),
        (status = 403, description = "The role lacks console.floor.manage", body = pos_cloud_api::error_schema::ErrorResponse),
        (status = 503, description = "The floor store or the entropy source is unavailable", body = pos_cloud_api::error_schema::ErrorResponse),
    ),
    tag = "floor",
)]
/// A super-admin creates a floor table; its id is minted here and returned once.
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

#[utoipa::path(
    patch,
    path = "/admin/floor/tables/{table_id}",
    extensions(("x-permission" = json!("console.floor.manage"))),
    security(("session_cookie" = [])),
    params(
        ("table_id" = String, Path, description = "The table (a 26-character ULID)"),
        ("If-Match" = String, Header, description = "The ETag the record was read at (ADR-0094). \
                                                     Required; a wildcard is refused"),
    ),
    request_body(
        content = UpdateTableRequest, description = "`tenant_id`, and the table's `area_id`, `name`, `seats`, `grid_column`, `grid_row` and `status` \
                       (`active` or `archived`) as they should now stand. A position counts only when \
                       both coordinates are given",
        content_type = "application/json",
    ),
    responses(
        (status = 204, description = "Updated. The body is empty and the ETag carries the new version. Audited"),
        (status = 400, description = "table_id, tenant_id or area_id is not a ULID, name is blank, status is not `active` or \
                                      `archived`, or If-Match is absent, malformed or a wildcard", body = pos_cloud_api::error_schema::ErrorResponse),
        (status = 401, description = "No session", body = pos_cloud_api::error_schema::ErrorResponse),
        (status = 403, description = "The role lacks console.floor.manage", body = pos_cloud_api::error_schema::ErrorResponse),
        (status = 404, description = "The tenant has no table with that id", body = pos_cloud_api::error_schema::ErrorResponse),
        (status = 412, description = "The table changed since you read it: re-read it and try again", body = pos_cloud_api::error_schema::ErrorResponse),
        (status = 503, description = "The floor store is unreachable", body = pos_cloud_api::error_schema::ErrorResponse),
    ),
    tag = "floor",
)]
/// A super-admin sets a floor table's whole new state, conditional on the version it was read at.
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
