// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! `/admin/kitchen/stations`: list, read, create and update a store's stations.

use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use pos_cloud_api::console::{audit_action, mint_ulid, require_permission};
use pos_cloud_api::response::{api_error_with_details, parse_ulid_fields, ulid_refusal};
use pos_proto::determinism::ClockSource;
use pos_proto::ids::{StationId, StoreId, TenantId};
use pos_proto::printing::ReceiptLanguage;
use pos_proto::{ErrorStatus, WireEnum};

use super::requests::{
    CreateStationRequest, FloorListQuery, FloorTenantQuery, UpdateStationRequest,
    station_late_after, station_ticket_language,
};
use super::{
    FloorState, floor_entropy_unavailable, floor_error_response, floor_tenant, floor_tenant_store,
};
use crate::auth::admin::AdminStore;
use crate::auth::console_rbac::ConsolePermission;
use crate::catalog::CatalogStore;
use crate::floorplan::{NewStation, StationStore, StationUpdate};
use crate::http::{
    Json, entity_status_refusal, if_match, not_found, parse_entity_status, parse_optional_ulid,
    version_mismatch, versioned_ok, with_etag,
};
use crate::registry::EntityStatus;
use crate::version::UpdateOutcome;

pub(super) async fn admin_list_stations<F, A, C, Cat>(
    State(state): State<FloorState<F, A, C, Cat>>,
    headers: HeaderMap,
    Query(query): Query<FloorListQuery>,
) -> Response
where
    F: StationStore + Clone + Send + Sync + 'static,
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
    match StationStore::list(&state.floor, tenant_id, store_id).await {
        Ok(stations) => (StatusCode::OK, Json(stations)).into_response(),
        Err(error) => floor_error_response(&error),
    }
}

pub(super) async fn admin_get_station<F, A, C, Cat>(
    State(state): State<FloorState<F, A, C, Cat>>,
    headers: HeaderMap,
    Path(station_id): Path<String>,
    Query(query): Query<FloorTenantQuery>,
) -> Response
where
    F: StationStore + Clone + Send + Sync + 'static,
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
    let station_id = match parse_ulid_fields([("station_id", &station_id)]) {
        Ok([station_id]) => StationId::new(station_id),
        Err(refusal) => return refusal,
    };
    match StationStore::get(&state.floor, tenant_id, station_id).await {
        Ok(Some(station)) => versioned_ok(station.record, &station.etag),
        Ok(None) => not_found("station"),
        Err(error) => floor_error_response(&error),
    }
}

pub(super) async fn admin_create_station<F, A, C, Cat>(
    State(state): State<FloorState<F, A, C, Cat>>,
    headers: HeaderMap,
    Json(request): Json<CreateStationRequest>,
) -> Response
where
    F: StationStore + Clone + Send + Sync + 'static,
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
    let Ok(backup_station_id) =
        parse_optional_ulid(request.backup_station_id.as_deref(), StationId::new)
    else {
        return ulid_refusal(&["backup_station_id"]);
    };
    let late_after_seconds = match station_late_after(request.late_after_seconds) {
        Ok(seconds) => seconds,
        Err(refusal) => return refusal,
    };
    let ticket_language = match station_ticket_language(request.ticket_language.as_deref()) {
        Ok(language) => language,
        Err(refusal) => return refusal,
    };
    let Some(station_id) =
        mint_ulid(state.clock.now().as_milliseconds_since_epoch()).map(StationId::new)
    else {
        return floor_entropy_unavailable();
    };
    let new_station = NewStation {
        station_id,
        tenant_id,
        store_id,
        name: request.name.clone(),
        backup_station_id,
        is_default: request.is_default,
        late_after_seconds,
        ticket_language,
    };
    match StationStore::create(&state.floor, &new_station).await {
        Ok(version) => {
            let after = serde_json::json!({
                "id": station_id.to_string(),
                "name": request.name,
                "is_default": request.is_default,
                "late_after_seconds": late_after_seconds,
                "ticket_language": ticket_language.map(ReceiptLanguage::as_wire),
                "status": EntityStatus::Active.as_str(),
            });
            audit_action(
                &state.audit,
                &state.clock,
                &context,
                Some(tenant_id),
                "kitchen.station.create",
                "kitchen_station",
                &station_id.to_string(),
                None,
                Some(after),
            )
            .await;
            with_etag(
                (
                    StatusCode::CREATED,
                    Json(serde_json::json!({ "id": station_id.to_string() })),
                )
                    .into_response(),
                &version,
            )
        }
        Err(error) => floor_error_response(&error),
    }
}

pub(super) async fn admin_update_station<F, A, C, Cat>(
    State(state): State<FloorState<F, A, C, Cat>>,
    headers: HeaderMap,
    Path(station_id): Path<String>,
    Json(request): Json<UpdateStationRequest>,
) -> Response
where
    F: StationStore + Clone + Send + Sync + 'static,
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
    let (tenant_id, station_id) = match parse_ulid_fields([
        ("tenant_id", &request.tenant_id),
        ("station_id", &station_id),
    ]) {
        Ok([tenant_id, station_id]) => (TenantId::new(tenant_id), StationId::new(station_id)),
        Err(refusal) => return refusal,
    };
    if request.name.trim().is_empty() {
        return api_error_with_details(
            ErrorStatus::InvalidArgument,
            "name is required",
            &[("name", "REQUIRED")],
        );
    }
    let Ok(backup_station_id) =
        parse_optional_ulid(request.backup_station_id.as_deref(), StationId::new)
    else {
        return ulid_refusal(&["backup_station_id"]);
    };
    let Some(status) = parse_entity_status(&request.status) else {
        return entity_status_refusal();
    };
    let late_after_seconds = match station_late_after(request.late_after_seconds) {
        Ok(seconds) => seconds,
        Err(refusal) => return refusal,
    };
    let ticket_language = match station_ticket_language(request.ticket_language.as_deref()) {
        Ok(language) => language,
        Err(refusal) => return refusal,
    };
    let update = StationUpdate {
        station_id,
        tenant_id,
        name: request.name.clone(),
        backup_station_id,
        is_default: request.is_default,
        late_after_seconds,
        ticket_language,
        status,
    };
    let expected = match if_match(&headers) {
        Ok(expected) => expected,
        Err(refusal) => return refusal,
    };
    match StationStore::update(&state.floor, &update, &expected).await {
        Ok(UpdateOutcome::Updated(version)) => {
            let after = serde_json::json!({
                "id": station_id.to_string(),
                "name": request.name,
                "is_default": request.is_default,
                "late_after_seconds": late_after_seconds,
                "ticket_language": ticket_language.map(ReceiptLanguage::as_wire),
                "status": status.as_str(),
            });
            audit_action(
                &state.audit,
                &state.clock,
                &context,
                Some(tenant_id),
                "kitchen.station.update",
                "kitchen_station",
                &station_id.to_string(),
                None,
                Some(after),
            )
            .await;
            with_etag(StatusCode::NO_CONTENT.into_response(), &version)
        }
        Ok(UpdateOutcome::VersionMismatch) => version_mismatch(),
        Ok(UpdateOutcome::NotFound) => not_found("station"),
        Err(error) => floor_error_response(&error),
    }
}
