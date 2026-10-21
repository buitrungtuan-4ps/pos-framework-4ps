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
use crate::registry::EntityStatus;
use crate::version::UpdateOutcome;
use axum::Json;
use pos_cloud_api::conditional::{if_match, version_mismatch, versioned_ok, with_etag};
use pos_cloud_api::refusal::{not_found, parse_optional_ulid};
use pos_cloud_config::route::{entity_status_refusal, parse_entity_status};

#[utoipa::path(
    get,
    path = "/admin/kitchen/stations",
    extensions(("x-permission" = json!("console.data.read"))),
    security(("session_cookie" = [])),
    params(
        ("tenant_id" = String, Query, description = "The store's tenant (a 26-character ULID)"),
        ("store_id" = String, Query, description = "The store (a 26-character ULID)"),
    ),
    responses(
        (status = 200, description = "The store's kitchen stations, active and archived, as an array, each with its `etag`"),
        (status = 400, description = "tenant_id or store_id is absent or not a ULID", body = pos_cloud_api::error_schema::ErrorResponse),
        (status = 401, description = "No session", body = pos_cloud_api::error_schema::ErrorResponse),
        (status = 403, description = "The role lacks console.data.read", body = pos_cloud_api::error_schema::ErrorResponse),
        (status = 503, description = "The floor store is unreachable", body = pos_cloud_api::error_schema::ErrorResponse),
    ),
    tag = "kitchen",
)]
/// A super-admin lists a store's kitchen stations.
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

#[utoipa::path(
    get,
    path = "/admin/kitchen/stations/{station_id}",
    extensions(("x-permission" = json!("console.data.read"))),
    security(("session_cookie" = [])),
    params(
        ("station_id" = String, Path, description = "The station (a 26-character ULID)"),
        ("tenant_id" = String, Query, description = "The tenant the record belongs to (a 26-character ULID)"),
    ),
    responses(
        (status = 200, description = "The station. The ETag carries the version it was read at"),
        (status = 400, description = "station_id or tenant_id is absent or not a ULID", body = pos_cloud_api::error_schema::ErrorResponse),
        (status = 401, description = "No session", body = pos_cloud_api::error_schema::ErrorResponse),
        (status = 403, description = "The role lacks console.data.read", body = pos_cloud_api::error_schema::ErrorResponse),
        (status = 404, description = "The tenant has no station with that id", body = pos_cloud_api::error_schema::ErrorResponse),
        (status = 503, description = "The floor store is unreachable", body = pos_cloud_api::error_schema::ErrorResponse),
    ),
    tag = "kitchen",
)]
/// A super-admin reads one kitchen station, with the version it was read at.
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

#[utoipa::path(
    post,
    path = "/admin/kitchen/stations",
    extensions(("x-permission" = json!("console.floor.manage"))),
    security(("session_cookie" = [])),
    request_body(
        description = "`tenant_id` and `store_id`, and the station's `name`, which may not be blank; optionally \
                       `backup_station_id`, the station its tickets go to when its printer is down; \
                       `is_default`, whether it is the store's catch-all for a fired line no rule \
                       routes; `late_after_seconds`, from 60 to 3600, when the kitchen display marks \
                       its tickets late, absent for the default; and \
                       `ticket_language`, a `RECEIPT_LANGUAGE_…` token, absent for the store's \
                       display language",
        content_type = "application/json",
    ),
    responses(
        (status = 201, description = "Created, active, with the id minted here. The body carries `id`, and the ETag carries \
                                      its version. Audited"),
        (status = 400, description = "tenant_id or store_id is absent or not a ULID; name is blank; backup_station_id is \
                                      not a ULID; late_after_seconds is outside 60 to 3600; or ticket_language is not \
                                      a language token", body = pos_cloud_api::error_schema::ErrorResponse),
        (status = 401, description = "No session", body = pos_cloud_api::error_schema::ErrorResponse),
        (status = 403, description = "The role lacks console.floor.manage", body = pos_cloud_api::error_schema::ErrorResponse),
        (status = 503, description = "The floor store or the entropy source is unavailable", body = pos_cloud_api::error_schema::ErrorResponse),
    ),
    tag = "kitchen",
)]
/// A super-admin creates a kitchen station; its id is minted here and returned once.
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

#[utoipa::path(
    patch,
    path = "/admin/kitchen/stations/{station_id}",
    extensions(("x-permission" = json!("console.floor.manage"))),
    security(("session_cookie" = [])),
    params(
        ("station_id" = String, Path, description = "The station (a 26-character ULID)"),
        ("If-Match" = String, Header, description = "The ETag the record was read at (ADR-0094). \
                                                     Required; a wildcard is refused"),
    ),
    request_body(
        description = "`tenant_id`, `status` (`active` or `archived`), and the station's `name`, which may not be blank; optionally \
                       `backup_station_id`, the station its tickets go to when its printer is down; \
                       `is_default`, whether it is the store's catch-all for a fired line no rule \
                       routes; `late_after_seconds`, from 60 to 3600, when the kitchen display marks \
                       its tickets late, absent for the default; and \
                       `ticket_language`, a `RECEIPT_LANGUAGE_…` token, absent for the store's \
                       display language. The body is the \
                       station's whole new state, so an absent threshold or language is the default again",
        content_type = "application/json",
    ),
    responses(
        (status = 204, description = "Updated. The body is empty and the ETag carries the new version. Audited"),
        (status = 400, description = "station_id or tenant_id is not a ULID; name is blank; backup_station_id is not a \
                                      ULID; late_after_seconds is outside 60 to 3600; ticket_language is not a \
                                      language token; status is not `active` or `archived`; or If-Match is absent, \
                                      malformed or a wildcard", body = pos_cloud_api::error_schema::ErrorResponse),
        (status = 401, description = "No session", body = pos_cloud_api::error_schema::ErrorResponse),
        (status = 403, description = "The role lacks console.floor.manage", body = pos_cloud_api::error_schema::ErrorResponse),
        (status = 404, description = "The tenant has no station with that id", body = pos_cloud_api::error_schema::ErrorResponse),
        (status = 412, description = "The station changed since you read it: re-read it and try again", body = pos_cloud_api::error_schema::ErrorResponse),
        (status = 503, description = "The floor store is unreachable", body = pos_cloud_api::error_schema::ErrorResponse),
    ),
    tag = "kitchen",
)]
/// A super-admin sets a kitchen station's whole new state, conditional on the version it was read at.
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
