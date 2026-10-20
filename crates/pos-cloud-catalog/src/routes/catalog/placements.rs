// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! `/admin/catalog/.../placements`: where an item sits on a menu.

use axum::Json;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use pos_cloud_api::console::{audit_action, require_permission};
use pos_cloud_api::response::{parse_ulid_fields, ulid_refusal};
use pos_proto::determinism::ClockSource;
use pos_proto::ids::{MenuItemId, TenantId};

use super::requests::{CreatePlacementRequest, SetPlacementRequest};
use super::{CatalogState, catalog_error_response};
use crate::auth::admin::AdminStore;
use crate::auth::console_rbac::ConsolePermission;
use crate::catalog::{CatalogStore, MenuId, MenuPlacement, MenuSectionId};
use crate::routes::{
    RegistryTenantQuery, already_exists, if_match, not_found, parse_optional_ulid,
    version_mismatch, versioned_created, versioned_ok,
};
use crate::version::{CreateOutcome, UpdateOutcome, Versioned};

#[utoipa::path(
    get,
    path = "/admin/catalog/menus/{menu_id}/placements",
    security(("session_cookie" = [])),
    params(
        ("menu_id" = String, Path, description = "The menu (a 26-character ULID)"),
        ("tenant_id" = String, Query, description = "The tenant that owns it (a 26-character ULID)"),
    ),
    responses(
        (status = 200, description = "The items on the menu, as an array: each with its section, \
                                      its price on every sales channel in minor units, whether it is \
                                      available, and its `etag`. Prices are T2, so this read needs \
                                      `console.reports.revenue` rather than the catalog's plain read"),
        (status = 400, description = "tenant_id or menu_id is absent or not a ULID", body = pos_cloud_api::error_schema::ErrorResponse),
        (status = 401, description = "No session", body = pos_cloud_api::error_schema::ErrorResponse),
        (status = 403, description = "The role lacks console.reports.revenue", body = pos_cloud_api::error_schema::ErrorResponse),
        (status = 503, description = "The catalog store is unreachable", body = pos_cloud_api::error_schema::ErrorResponse),
    ),
    tag = "menus",
)]
/// A super-admin lists a menu's placements (tenant named on the query).
///
/// Behind [`ConsolePermission::ReadRevenue`], not plain `Read` (production-readiness **S5**): a
/// [`MenuPlacement`] carries `prices` — the item's price on every channel of this menu — which is
/// exactly the commercially sensitive (**T2**) data `ReadRevenue` was carved out of `Read` to hold
/// back. Until this row was verified the console handed a store's full per-channel price book to any
/// Viewer, while refusing the same figures on a revenue report.
///
/// The consequence is deliberate and worth stating: Ops and Viewer can still list the menus, their
/// sections and the item master, and can no longer read what anything costs.
pub(super) async fn admin_list_placements<Cat, A, C>(
    State(state): State<CatalogState<Cat, A, C>>,
    headers: HeaderMap,
    Path(menu_id): Path<String>,
    Query(query): Query<RegistryTenantQuery>,
) -> Response
where
    Cat: CatalogStore + Clone + Send + Sync + 'static,
    A: AdminStore + Clone + Send + Sync + 'static,
    C: ClockSource + Clone + Send + Sync + 'static,
{
    if let Err(denied) = require_permission(
        &state.admin,
        &state.clock,
        &headers,
        ConsolePermission::ReadRevenue,
    )
    .await
    {
        return denied;
    }
    let (tenant_id, menu_id) =
        match parse_ulid_fields([("tenant_id", &query.tenant_id), ("menu_id", &menu_id)]) {
            Ok([tenant_id, menu_id]) => (TenantId::new(tenant_id), MenuId::new(menu_id)),
            Err(refusal) => return refusal,
        };
    match state.catalog.list_placements(tenant_id, menu_id).await {
        Ok(rows) => (StatusCode::OK, Json::<Vec<Versioned<MenuPlacement>>>(rows)).into_response(),
        Err(error) => catalog_error_response(&error),
    }
}

/// Assembles a [`MenuPlacement`] from a request plus the menu and item that identify it.
///
/// Shared by the create and update routes, for the same reason as `build_layout_button`.
#[expect(
    clippy::result_large_err,
    reason = "the Err is an axum Response by design — it *is* the 400 the caller returns, the shape `parse_ulid_fields` already carries this expectation for"
)]
fn build_placement(
    request: SetPlacementRequest,
    menu_id: &str,
    menu_item_id: &str,
) -> Result<MenuPlacement, Response> {
    let (tenant_id, menu_id, menu_item_id) = match parse_ulid_fields([
        ("tenant_id", &request.tenant_id),
        ("menu_id", menu_id),
        ("menu_item_id", menu_item_id),
    ]) {
        Ok([tenant_id, menu_id, menu_item_id]) => (
            TenantId::new(tenant_id),
            MenuId::new(menu_id),
            MenuItemId::new(menu_item_id),
        ),
        Err(refusal) => return Err(refusal),
    };
    let Ok(menu_section_id) =
        parse_optional_ulid(request.menu_section_id.as_deref(), MenuSectionId::new)
    else {
        return Err(ulid_refusal(&["menu_section_id"]));
    };
    Ok(MenuPlacement {
        tenant_id,
        menu_id,
        menu_item_id,
        menu_section_id,
        prices: request.prices,
        available: request.available,
    })
}

#[utoipa::path(
    post,
    path = "/admin/catalog/menus/{menu_id}/placements",
    security(("session_cookie" = [])),
    params(
        ("menu_id" = String, Path, description = "The menu (a 26-character ULID)"),
    ),
    request_body(
        description = "`menu_id`, `menu_item_id` and `tenant_id`, then `menu_section_id` (optional), `prices`, one per sales channel, each a `sales_channel` \
                       token and a `unit_price` in the currency's minor units, and `available`. An item is on a menu \
                       at most once",
        content_type = "application/json",
    ),
    responses(
        (status = 201, description = "Placed. The body is the placement and the ETag carries its \
                                      version. Audited with its prices, the price-change journal \
                                      (ADR-0069)"),
        (status = 400, description = "An id is not a ULID", body = pos_cloud_api::error_schema::ErrorResponse),
        (status = 401, description = "No session", body = pos_cloud_api::error_schema::ErrorResponse),
        (status = 403, description = "The role lacks console.catalog.manage", body = pos_cloud_api::error_schema::ErrorResponse),
        (status = 409, description = "The item is already on this menu: change it with PUT instead", body = pos_cloud_api::error_schema::ErrorResponse),
        (status = 503, description = "The catalog store is unreachable", body = pos_cloud_api::error_schema::ErrorResponse),
    ),
    tag = "menus",
)]
/// A super-admin adds an item to a menu, refusing if it is already on it.
///
/// The route this replaced was a single `PUT` upsert. A placement's identity is the caller-supplied
/// `(menu, item)` pair, so "add this item" silently replaced the prices, section and availability
/// of one already on the menu — and because the per-channel prices are the price-change journal
/// (ADR-0069, G2), the overwrite was recorded as a `set` with no `before` to compare against.
pub(super) async fn admin_create_placement<Cat, A, C>(
    State(state): State<CatalogState<Cat, A, C>>,
    headers: HeaderMap,
    Json(request): Json<CreatePlacementRequest>,
) -> Response
where
    Cat: CatalogStore + Clone + Send + Sync + 'static,
    A: AdminStore + Clone + Send + Sync + 'static,
    C: ClockSource + Clone + Send + Sync + 'static,
{
    let context = match require_permission(
        &state.admin,
        &state.clock,
        &headers,
        ConsolePermission::ManageCatalog,
    )
    .await
    {
        Ok(context) => context,
        Err(denied) => return denied,
    };
    let (menu_id, menu_item_id) = (request.menu_id.clone(), request.menu_item_id.clone());
    let record = match build_placement(request.into_write(), &menu_id, &menu_item_id) {
        Ok(record) => record,
        Err(refusal) => return refusal,
    };
    match state.catalog.create_placement(&record).await {
        Ok(CreateOutcome::Created(version)) => {
            let entity_id = format!("{}/{}", record.menu_id, record.menu_item_id);
            audit_action(
                &state.audit,
                &state.clock,
                &context,
                Some(record.tenant_id),
                "placement.create",
                "menu_placement",
                &entity_id,
                None,
                serde_json::to_value(&record).ok(),
            )
            .await;
            versioned_created(record, &version)
        }
        Ok(CreateOutcome::AlreadyExists) => already_exists("menu placement"),
        Err(error) => catalog_error_response(&error),
    }
}

#[utoipa::path(
    put,
    path = "/admin/catalog/menus/{menu_id}/placements/{menu_item_id}",
    security(("session_cookie" = [])),
    params(
        ("menu_id" = String, Path, description = "The menu (a 26-character ULID)"),
        ("menu_item_id" = String, Path, description = "The item (a 26-character ULID)"),
        ("If-Match" = String, Header, description = "The ETag the record was read at (ADR-0094). \
                                                     Required; a wildcard is refused"),
    ),
    request_body(
        description = "`tenant_id`, then `menu_section_id` (optional), `prices`, one per sales channel, each a `sales_channel` \
                       token and a `unit_price` in the currency's minor units, and `available`, as they should now stand",
        content_type = "application/json",
    ),
    responses(
        (status = 200, description = "Updated. The body is the placement and the ETag carries its \
                                      new version. Audited with its prices"),
        (status = 400, description = "An id is not a ULID, or If-Match is absent, malformed, or a \
                                      wildcard", body = pos_cloud_api::error_schema::ErrorResponse),
        (status = 401, description = "No session", body = pos_cloud_api::error_schema::ErrorResponse),
        (status = 403, description = "The role lacks console.catalog.manage", body = pos_cloud_api::error_schema::ErrorResponse),
        (status = 404, description = "The item is not on this menu", body = pos_cloud_api::error_schema::ErrorResponse),
        (status = 412, description = "The placement changed since you read it: re-read it and try \
                                      again", body = pos_cloud_api::error_schema::ErrorResponse),
        (status = 503, description = "The catalog store is unreachable", body = pos_cloud_api::error_schema::ErrorResponse),
    ),
    tag = "menus",
)]
/// A super-admin changes an item's prices and availability on a menu, only at the version they read.
pub(super) async fn admin_update_placement<Cat, A, C>(
    State(state): State<CatalogState<Cat, A, C>>,
    headers: HeaderMap,
    Path((menu_id, menu_item_id)): Path<(String, String)>,
    Json(request): Json<SetPlacementRequest>,
) -> Response
where
    Cat: CatalogStore + Clone + Send + Sync + 'static,
    A: AdminStore + Clone + Send + Sync + 'static,
    C: ClockSource + Clone + Send + Sync + 'static,
{
    let context = match require_permission(
        &state.admin,
        &state.clock,
        &headers,
        ConsolePermission::ManageCatalog,
    )
    .await
    {
        Ok(context) => context,
        Err(denied) => return denied,
    };
    let record = match build_placement(request, &menu_id, &menu_item_id) {
        Ok(record) => record,
        Err(refusal) => return refusal,
    };
    let expected = match if_match(&headers) {
        Ok(expected) => expected,
        Err(refusal) => return refusal,
    };
    match state.catalog.update_placement(&record, &expected).await {
        Ok(UpdateOutcome::Updated(version)) => {
            // A placement's identity is the (menu, item) pair; its per-channel prices are the
            // price-change journal (ADR-0069, G2), recorded as `after`.
            let entity_id = format!("{}/{}", record.menu_id, record.menu_item_id);
            audit_action(
                &state.audit,
                &state.clock,
                &context,
                Some(record.tenant_id),
                "placement.update",
                "menu_placement",
                &entity_id,
                None,
                serde_json::to_value(&record).ok(),
            )
            .await;
            versioned_ok(record, &version)
        }
        Ok(UpdateOutcome::VersionMismatch) => version_mismatch(),
        Ok(UpdateOutcome::NotFound) => not_found("menu placement"),
        Err(error) => catalog_error_response(&error),
    }
}

#[utoipa::path(
    delete,
    path = "/admin/catalog/menus/{menu_id}/placements/{menu_item_id}",
    security(("session_cookie" = [])),
    params(
        ("menu_id" = String, Path, description = "The menu (a 26-character ULID)"),
        ("menu_item_id" = String, Path, description = "The item (a 26-character ULID)"),
        ("tenant_id" = String, Query, description = "The tenant that owns it (a 26-character ULID)"),
    ),
    responses(
        (status = 204, description = "Removed from the menu, and audited"),
        (status = 400, description = "An id is absent or not a ULID", body = pos_cloud_api::error_schema::ErrorResponse),
        (status = 401, description = "No session", body = pos_cloud_api::error_schema::ErrorResponse),
        (status = 403, description = "The role lacks console.catalog.manage", body = pos_cloud_api::error_schema::ErrorResponse),
        (status = 404, description = "The item is not on this menu", body = pos_cloud_api::error_schema::ErrorResponse),
        (status = 503, description = "The catalog store is unreachable", body = pos_cloud_api::error_schema::ErrorResponse),
    ),
    tag = "menus",
)]
/// A super-admin removes an item from a menu (tenant named on the query).
pub(super) async fn admin_remove_placement<Cat, A, C>(
    State(state): State<CatalogState<Cat, A, C>>,
    headers: HeaderMap,
    Path((menu_id, menu_item_id)): Path<(String, String)>,
    Query(query): Query<RegistryTenantQuery>,
) -> Response
where
    Cat: CatalogStore + Clone + Send + Sync + 'static,
    A: AdminStore + Clone + Send + Sync + 'static,
    C: ClockSource + Clone + Send + Sync + 'static,
{
    let context = match require_permission(
        &state.admin,
        &state.clock,
        &headers,
        ConsolePermission::ManageCatalog,
    )
    .await
    {
        Ok(context) => context,
        Err(denied) => return denied,
    };
    let (tenant_id, menu_id, menu_item_id) = match parse_ulid_fields([
        ("tenant_id", &query.tenant_id),
        ("menu_id", &menu_id),
        ("menu_item_id", &menu_item_id),
    ]) {
        Ok([tenant_id, menu_id, menu_item_id]) => (
            TenantId::new(tenant_id),
            MenuId::new(menu_id),
            MenuItemId::new(menu_item_id),
        ),
        Err(refusal) => return refusal,
    };
    match state
        .catalog
        .remove_placement(tenant_id, menu_id, menu_item_id)
        .await
    {
        Ok(true) => {
            let entity_id = format!("{menu_id}/{menu_item_id}");
            audit_action(
                &state.audit,
                &state.clock,
                &context,
                Some(tenant_id),
                "placement.remove",
                "menu_placement",
                &entity_id,
                None,
                None,
            )
            .await;
            StatusCode::NO_CONTENT.into_response()
        }
        Ok(false) => not_found("placement"),
        Err(error) => catalog_error_response(&error),
    }
}
