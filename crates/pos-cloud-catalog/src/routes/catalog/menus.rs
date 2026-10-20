// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! `/admin/catalog/menus` and their sections.

use axum::Json;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use pos_cloud_api::console::{audit_action, mint_ulid, require_permission};
use pos_cloud_api::response::{parse_ulid_fields, ulid_refusal};
use pos_proto::determinism::ClockSource;
use pos_proto::ids::TenantId;

use super::requests::{
    CreateMenuRequest, CreateMenuSectionRequest, UpdateMenuRequest, UpdateMenuSectionRequest,
};
use super::{CatalogState, catalog_entropy_unavailable, catalog_error_response};
use crate::auth::admin::AdminStore;
use crate::auth::console_rbac::ConsolePermission;
use crate::catalog::{CatalogStore, Menu, MenuId, MenuSection, MenuSectionId};
use crate::catalog_compiler::would_cycle;
use crate::registry::EntityStatus;
use crate::routes::{
    RegistryTenantQuery, entity_status_refusal, if_match, not_found, parse_entity_status,
    parse_optional_ulid, unprocessable_violations, version_mismatch, versioned_created,
    versioned_ok,
};
use crate::version::UpdateOutcome;

#[utoipa::path(
    get,
    path = "/admin/catalog/menus",
    security(("session_cookie" = [])),
    params(
        ("tenant_id" = String, Query, description = "The tenant whose menus to list (a 26-character ULID)"),
    ),
    responses(
        (status = 200, description = "The tenant's menus, as an array, each with its \
                                      `parent_menu_id` (absent for a menu that inherits from none) \
                                      and its `etag`"),
        (status = 400, description = "tenant_id is absent or not a ULID", body = pos_cloud_api::error_schema::ErrorResponse),
        (status = 401, description = "No session", body = pos_cloud_api::error_schema::ErrorResponse),
        (status = 403, description = "The role lacks console.data.read", body = pos_cloud_api::error_schema::ErrorResponse),
        (status = 503, description = "The catalog store is unreachable", body = pos_cloud_api::error_schema::ErrorResponse),
    ),
    tag = "menus",
)]
/// A super-admin lists a tenant's menus.
pub(super) async fn admin_list_menus<Cat, A, C>(
    State(state): State<CatalogState<Cat, A, C>>,
    headers: HeaderMap,
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
    match state.catalog.list_menus(tenant_id).await {
        Ok(menus) => (StatusCode::OK, Json(menus)).into_response(),
        Err(error) => catalog_error_response(&error),
    }
}

#[utoipa::path(
    post,
    path = "/admin/catalog/menus",
    security(("session_cookie" = [])),
    request_body(
        description = "`tenant_id`, the menu's `name`, and optionally `parent_menu_id`, a menu it \
                       inherits its placements from",
        content_type = "application/json",
    ),
    responses(
        (status = 201, description = "Created, active, with the id minted here. The body is the \
                                      record and the ETag carries its version. Audited"),
        (status = 400, description = "tenant_id or parent_menu_id is not a ULID", body = pos_cloud_api::error_schema::ErrorResponse),
        (status = 401, description = "No session", body = pos_cloud_api::error_schema::ErrorResponse),
        (status = 403, description = "The role lacks console.catalog.manage", body = pos_cloud_api::error_schema::ErrorResponse),
        (status = 503, description = "The catalog store or the entropy source is unavailable", body = pos_cloud_api::error_schema::ErrorResponse),
    ),
    tag = "menus",
)]
/// A super-admin creates a menu, optionally under a parent it inherits from.
pub(super) async fn admin_create_menu<Cat, A, C>(
    State(state): State<CatalogState<Cat, A, C>>,
    headers: HeaderMap,
    Json(request): Json<CreateMenuRequest>,
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
    let tenant_id = match parse_ulid_fields([("tenant_id", &request.tenant_id)]) {
        Ok([tenant_id]) => TenantId::new(tenant_id),
        Err(refusal) => return refusal,
    };
    let Ok(parent_menu_id) = parse_optional_ulid(request.parent_menu_id.as_deref(), MenuId::new)
    else {
        return ulid_refusal(&["parent_menu_id"]);
    };
    let Some(menu_id) = mint_ulid(state.clock.now().as_milliseconds_since_epoch()).map(MenuId::new)
    else {
        return catalog_entropy_unavailable();
    };
    let record = Menu {
        menu_id,
        tenant_id,
        name: request.name,
        parent_menu_id,
        status: EntityStatus::Active,
    };
    match state.catalog.create_menu(&record).await {
        Ok(version) => {
            audit_action(
                &state.audit,
                &state.clock,
                &context,
                Some(record.tenant_id),
                "menu.create",
                "menu",
                &record.menu_id.to_string(),
                None,
                serde_json::to_value(&record).ok(),
            )
            .await;
            versioned_created(record, &version)
        }
        Err(error) => catalog_error_response(&error),
    }
}

#[utoipa::path(
    patch,
    path = "/admin/catalog/menus/{menu_id}",
    security(("session_cookie" = [])),
    params(
        ("menu_id" = String, Path, description = "The menu (a 26-character ULID)"),
        ("If-Match" = String, Header, description = "The ETag the record was read at (ADR-0094). \
                                                     Required; a wildcard is refused"),
    ),
    request_body(
        description = "`tenant_id`, and the menu's `name`, `parent_menu_id` (absent clears it) and \
                       `status` (`active` or `archived`) as they should now stand",
        content_type = "application/json",
    ),
    responses(
        (status = 200, description = "Updated. The body is the record and the ETag carries its new \
                                      version. Audited"),
        (status = 400, description = "menu_id, tenant_id or parent_menu_id is not a ULID, status is \
                                      not `active` or `archived`, or If-Match is absent, malformed, \
                                      or a wildcard", body = pos_cloud_api::error_schema::ErrorResponse),
        (status = 401, description = "No session", body = pos_cloud_api::error_schema::ErrorResponse),
        (status = 403, description = "The role lacks console.catalog.manage", body = pos_cloud_api::error_schema::ErrorResponse),
        (status = 404, description = "The tenant has no menu with that id", body = pos_cloud_api::error_schema::ErrorResponse),
        (status = 412, description = "The menu changed since you read it: re-read it and try again", body = pos_cloud_api::error_schema::ErrorResponse),
        (status = 422, description = "The parent would make the inheritance chain loop", body = pos_cloud_api::error_schema::ErrorResponse),
        (status = 503, description = "The catalog store is unreachable", body = pos_cloud_api::error_schema::ErrorResponse),
    ),
    tag = "menus",
)]
/// A super-admin renames a menu, (re)sets its parent and/or status.
pub(super) async fn admin_update_menu<Cat, A, C>(
    State(state): State<CatalogState<Cat, A, C>>,
    headers: HeaderMap,
    Path(menu_id): Path<String>,
    Json(request): Json<UpdateMenuRequest>,
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
    let (menu_id, tenant_id) =
        match parse_ulid_fields([("menu_id", &menu_id), ("tenant_id", &request.tenant_id)]) {
            Ok([menu_id, tenant_id]) => (MenuId::new(menu_id), TenantId::new(tenant_id)),
            Err(refusal) => return refusal,
        };
    let Ok(parent_menu_id) = parse_optional_ulid(request.parent_menu_id.as_deref(), MenuId::new)
    else {
        return ulid_refusal(&["parent_menu_id"]);
    };
    let Some(status) = parse_entity_status(&request.status) else {
        return entity_status_refusal();
    };
    // A parent that loops is refused here rather than at publish. The compiler still catches a
    // cycle — it has to, the stored graph may already hold one — but by then the operator is
    // several screens away from the dropdown that caused it, reading a `422` about a menu they were
    // not editing. This is the read the check costs: the tenant's menus, only when a parent is set.
    if let Some(parent) = parent_menu_id {
        match state.catalog.list_menus(tenant_id).await {
            Ok(rows) => {
                let menus: Vec<Menu> = rows.into_iter().map(|row| row.record).collect();
                if would_cycle(&menus, menu_id, parent) {
                    return unprocessable_violations(&[format!(
                        "menu {menu_id} cannot inherit from {parent}: that would make the inheritance chain loop"
                    )]);
                }
            }
            Err(error) => return catalog_error_response(&error),
        }
    }
    let record = Menu {
        menu_id,
        tenant_id,
        name: request.name,
        parent_menu_id,
        status,
    };
    let expected = match if_match(&headers) {
        Ok(expected) => expected,
        Err(refusal) => return refusal,
    };
    match state.catalog.update_menu(&record, &expected).await {
        Ok(UpdateOutcome::Updated(version)) => {
            audit_action(
                &state.audit,
                &state.clock,
                &context,
                Some(record.tenant_id),
                "menu.update",
                "menu",
                &record.menu_id.to_string(),
                None,
                serde_json::to_value(&record).ok(),
            )
            .await;
            versioned_ok(record, &version)
        }
        Ok(UpdateOutcome::VersionMismatch) => version_mismatch(),
        Ok(UpdateOutcome::NotFound) => not_found("menu"),
        Err(error) => catalog_error_response(&error),
    }
}

#[utoipa::path(
    get,
    path = "/admin/catalog/menus/{menu_id}/sections",
    security(("session_cookie" = [])),
    params(
        ("menu_id" = String, Path, description = "The menu (a 26-character ULID)"),
        ("tenant_id" = String, Query, description = "The tenant that owns it (a 26-character ULID)"),
    ),
    responses(
        (status = 200, description = "The menu's sections, as an array, each with its `sort` and \
                                      `etag`"),
        (status = 400, description = "tenant_id or menu_id is absent or not a ULID", body = pos_cloud_api::error_schema::ErrorResponse),
        (status = 401, description = "No session", body = pos_cloud_api::error_schema::ErrorResponse),
        (status = 403, description = "The role lacks console.data.read", body = pos_cloud_api::error_schema::ErrorResponse),
        (status = 503, description = "The catalog store is unreachable", body = pos_cloud_api::error_schema::ErrorResponse),
    ),
    tag = "menus",
)]
/// A super-admin lists a menu's sections (tenant named on the query).
pub(super) async fn admin_list_menu_sections<Cat, A, C>(
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
        ConsolePermission::Read,
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
    match state.catalog.list_menu_sections(tenant_id, menu_id).await {
        Ok(rows) => (StatusCode::OK, Json(rows)).into_response(),
        Err(error) => catalog_error_response(&error),
    }
}

#[utoipa::path(
    post,
    path = "/admin/catalog/menus/{menu_id}/sections",
    security(("session_cookie" = [])),
    params(
        ("menu_id" = String, Path, description = "The menu (a 26-character ULID)"),
    ),
    request_body(
        description = "`tenant_id`, the section's `name`, and `sort`, where it falls on the menu, \
                       ascending (absent is 0)",
        content_type = "application/json",
    ),
    responses(
        (status = 201, description = "Created, active, with the id minted here. The body is the \
                                      record and the ETag carries its version. Audited"),
        (status = 400, description = "tenant_id or menu_id is not a ULID", body = pos_cloud_api::error_schema::ErrorResponse),
        (status = 401, description = "No session", body = pos_cloud_api::error_schema::ErrorResponse),
        (status = 403, description = "The role lacks console.catalog.manage", body = pos_cloud_api::error_schema::ErrorResponse),
        (status = 503, description = "The catalog store or the entropy source is unavailable", body = pos_cloud_api::error_schema::ErrorResponse),
    ),
    tag = "menus",
)]
/// A super-admin creates a section within a menu.
pub(super) async fn admin_create_menu_section<Cat, A, C>(
    State(state): State<CatalogState<Cat, A, C>>,
    headers: HeaderMap,
    Path(menu_id): Path<String>,
    Json(request): Json<CreateMenuSectionRequest>,
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
    let (tenant_id, menu_id) =
        match parse_ulid_fields([("tenant_id", &request.tenant_id), ("menu_id", &menu_id)]) {
            Ok([tenant_id, menu_id]) => (TenantId::new(tenant_id), MenuId::new(menu_id)),
            Err(refusal) => return refusal,
        };
    let Some(menu_section_id) =
        mint_ulid(state.clock.now().as_milliseconds_since_epoch()).map(MenuSectionId::new)
    else {
        return catalog_entropy_unavailable();
    };
    let record = MenuSection {
        menu_section_id,
        tenant_id,
        menu_id,
        name: request.name,
        sort: request.sort,
        status: EntityStatus::Active,
    };
    match state.catalog.create_menu_section(&record).await {
        Ok(version) => {
            audit_action(
                &state.audit,
                &state.clock,
                &context,
                Some(record.tenant_id),
                "menu_section.create",
                "menu_section",
                &record.menu_section_id.to_string(),
                None,
                serde_json::to_value(&record).ok(),
            )
            .await;
            versioned_created(record, &version)
        }
        Err(error) => catalog_error_response(&error),
    }
}

#[utoipa::path(
    patch,
    path = "/admin/catalog/menus/{menu_id}/sections/{menu_section_id}",
    security(("session_cookie" = [])),
    params(
        ("menu_id" = String, Path, description = "The menu (a 26-character ULID)"),
        ("menu_section_id" = String, Path, description = "The section (a 26-character ULID)"),
        ("If-Match" = String, Header, description = "The ETag the record was read at (ADR-0094). \
                                                     Required; a wildcard is refused"),
    ),
    request_body(
        description = "`tenant_id`, and the section's `name`, `sort` and `status` (`active` or \
                       `archived`) as they should now stand",
        content_type = "application/json",
    ),
    responses(
        (status = 200, description = "Updated. The body is the record and the ETag carries its new \
                                      version. Audited"),
        (status = 400, description = "An id is not a ULID, status is not `active` or `archived`, or \
                                      If-Match is absent, malformed, or a wildcard", body = pos_cloud_api::error_schema::ErrorResponse),
        (status = 401, description = "No session", body = pos_cloud_api::error_schema::ErrorResponse),
        (status = 403, description = "The role lacks console.catalog.manage", body = pos_cloud_api::error_schema::ErrorResponse),
        (status = 404, description = "The menu has no section with that id", body = pos_cloud_api::error_schema::ErrorResponse),
        (status = 412, description = "The section changed since you read it: re-read it and try \
                                      again", body = pos_cloud_api::error_schema::ErrorResponse),
        (status = 503, description = "The catalog store is unreachable", body = pos_cloud_api::error_schema::ErrorResponse),
    ),
    tag = "menus",
)]
/// A super-admin renames a menu section, sets its sort and/or status.
pub(super) async fn admin_update_menu_section<Cat, A, C>(
    State(state): State<CatalogState<Cat, A, C>>,
    headers: HeaderMap,
    Path((menu_id, menu_section_id)): Path<(String, String)>,
    Json(request): Json<UpdateMenuSectionRequest>,
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
    let (tenant_id, menu_id, menu_section_id) = match parse_ulid_fields([
        ("tenant_id", &request.tenant_id),
        ("menu_id", &menu_id),
        ("menu_section_id", &menu_section_id),
    ]) {
        Ok([tenant_id, menu_id, menu_section_id]) => (
            TenantId::new(tenant_id),
            MenuId::new(menu_id),
            MenuSectionId::new(menu_section_id),
        ),
        Err(refusal) => return refusal,
    };
    let Some(status) = parse_entity_status(&request.status) else {
        return entity_status_refusal();
    };
    let record = MenuSection {
        menu_section_id,
        tenant_id,
        menu_id,
        name: request.name,
        sort: request.sort,
        status,
    };
    let expected = match if_match(&headers) {
        Ok(expected) => expected,
        Err(refusal) => return refusal,
    };
    match state.catalog.update_menu_section(&record, &expected).await {
        Ok(UpdateOutcome::Updated(version)) => {
            audit_action(
                &state.audit,
                &state.clock,
                &context,
                Some(record.tenant_id),
                "menu_section.update",
                "menu_section",
                &record.menu_section_id.to_string(),
                None,
                serde_json::to_value(&record).ok(),
            )
            .await;
            versioned_ok(record, &version)
        }
        Ok(UpdateOutcome::VersionMismatch) => version_mismatch(),
        Ok(UpdateOutcome::NotFound) => not_found("menu section"),
        Err(error) => catalog_error_response(&error),
    }
}
