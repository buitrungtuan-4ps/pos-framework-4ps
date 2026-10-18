// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! `/admin/floor/areas`: list, read, create and update a store's areas.

use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use pos_cloud_api::console::{audit_action, mint_ulid, require_permission};
use pos_cloud_api::response::{api_error_with_details, parse_ulid_fields};
use pos_proto::ErrorStatus;
use pos_proto::determinism::ClockSource;
use pos_proto::ids::{AreaId, StoreId, TenantId};

use super::requests::{CreateAreaRequest, FloorListQuery, FloorTenantQuery, UpdateAreaRequest};
use super::{
    FloorState, floor_entropy_unavailable, floor_error_response, floor_tenant, floor_tenant_store,
};
use crate::auth::admin::AdminStore;
use crate::auth::console_rbac::ConsolePermission;
use crate::catalog::CatalogStore;
use crate::floorplan::{AreaStore, AreaUpdate, NewArea};
use crate::registry::EntityStatus;
use crate::version::UpdateOutcome;
use axum::Json;
use pos_cloud_api::conditional::{if_match, version_mismatch, versioned_ok, with_etag};
use pos_cloud_api::refusal::not_found;
use pos_cloud_config::route::{entity_status_refusal, parse_entity_status};

pub(super) async fn admin_list_areas<F, A, C, Cat>(
    State(state): State<FloorState<F, A, C, Cat>>,
    headers: HeaderMap,
    Query(query): Query<FloorListQuery>,
) -> Response
where
    F: AreaStore + Clone + Send + Sync + 'static,
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
    match AreaStore::list(&state.floor, tenant_id, store_id).await {
        Ok(areas) => (StatusCode::OK, Json(areas)).into_response(),
        Err(error) => floor_error_response(&error),
    }
}

pub(super) async fn admin_get_area<F, A, C, Cat>(
    State(state): State<FloorState<F, A, C, Cat>>,
    headers: HeaderMap,
    Path(area_id): Path<String>,
    Query(query): Query<FloorTenantQuery>,
) -> Response
where
    F: AreaStore + Clone + Send + Sync + 'static,
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
    let area_id = match parse_ulid_fields([("area_id", &area_id)]) {
        Ok([area_id]) => AreaId::new(area_id),
        Err(refusal) => return refusal,
    };
    match AreaStore::get(&state.floor, tenant_id, area_id).await {
        Ok(Some(area)) => versioned_ok(area.record, &area.etag),
        Ok(None) => not_found("area"),
        Err(error) => floor_error_response(&error),
    }
}

pub(super) async fn admin_create_area<F, A, C, Cat>(
    State(state): State<FloorState<F, A, C, Cat>>,
    headers: HeaderMap,
    Json(request): Json<CreateAreaRequest>,
) -> Response
where
    F: AreaStore + Clone + Send + Sync + 'static,
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
    let (tenant_id, store_id) = match parse_ulid_fields([
        ("tenant_id", &request.tenant_id),
        ("store_id", &request.store_id),
    ]) {
        Ok([tenant_id, store_id]) => (TenantId::new(tenant_id), StoreId::new(store_id)),
        Err(refusal) => return refusal,
    };
    if request.name.trim().is_empty() {
        return api_error_with_details(
            ErrorStatus::InvalidArgument,
            "name is required",
            &[("name", "REQUIRED")],
        );
    }
    let Some(area_id) = mint_ulid(state.clock.now().as_milliseconds_since_epoch()).map(AreaId::new)
    else {
        return floor_entropy_unavailable();
    };
    let new_area = NewArea {
        area_id,
        tenant_id,
        store_id,
        name: request.name.clone(),
    };
    match AreaStore::create(&state.floor, &new_area).await {
        Ok(version) => {
            let after = serde_json::json!({
                "id": area_id.to_string(),
                "store_id": store_id.to_string(),
                "name": request.name,
                "status": EntityStatus::Active.as_str(),
            });
            audit_action(
                &state.audit,
                &state.clock,
                &context,
                Some(tenant_id),
                "floor.area.create",
                "floor_area",
                &area_id.to_string(),
                None,
                Some(after),
            )
            .await;
            with_etag(
                (
                    StatusCode::CREATED,
                    Json(serde_json::json!({ "id": area_id.to_string() })),
                )
                    .into_response(),
                &version,
            )
        }
        Err(error) => floor_error_response(&error),
    }
}

pub(super) async fn admin_update_area<F, A, C, Cat>(
    State(state): State<FloorState<F, A, C, Cat>>,
    headers: HeaderMap,
    Path(area_id): Path<String>,
    Json(request): Json<UpdateAreaRequest>,
) -> Response
where
    F: AreaStore + Clone + Send + Sync + 'static,
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
    let (tenant_id, area_id) =
        match parse_ulid_fields([("tenant_id", &request.tenant_id), ("area_id", &area_id)]) {
            Ok([tenant_id, area_id]) => (TenantId::new(tenant_id), AreaId::new(area_id)),
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
    let update = AreaUpdate {
        area_id,
        tenant_id,
        name: request.name.clone(),
        status,
    };
    let expected = match if_match(&headers) {
        Ok(expected) => expected,
        Err(refusal) => return refusal,
    };
    match AreaStore::update(&state.floor, &update, &expected).await {
        Ok(UpdateOutcome::Updated(version)) => {
            let after = serde_json::json!({
                "id": area_id.to_string(),
                "name": request.name,
                "status": status.as_str(),
            });
            audit_action(
                &state.audit,
                &state.clock,
                &context,
                Some(tenant_id),
                "floor.area.update",
                "floor_area",
                &area_id.to_string(),
                None,
                Some(after),
            )
            .await;
            with_etag(StatusCode::NO_CONTENT.into_response(), &version)
        }
        Ok(UpdateOutcome::VersionMismatch) => version_mismatch(),
        Ok(UpdateOutcome::NotFound) => not_found("area"),
        Err(error) => floor_error_response(&error),
    }
}
