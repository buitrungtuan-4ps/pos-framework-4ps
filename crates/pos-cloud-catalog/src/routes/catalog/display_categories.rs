// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! `/admin/catalog/display-categories` and `/admin/catalog/display-subcategories`.

use axum::Json;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use pos_cloud_api::console::{audit_action, mint_ulid, require_permission};
use pos_cloud_api::response::parse_ulid_fields;
use pos_proto::determinism::ClockSource;
use pos_proto::ids::{DisplayCategoryId, DisplaySubcategoryId, TenantId};

use super::requests::{
    CreateDisplayCategoryRequest, CreateDisplaySubcategoryRequest, UpdateDisplayCategoryRequest,
    UpdateDisplaySubcategoryRequest,
};
use super::{CatalogState, catalog_entropy_unavailable, catalog_error_response};
use crate::auth::admin::AdminStore;
use crate::auth::console_rbac::ConsolePermission;
use crate::catalog::{CatalogStore, DisplayCategory, DisplaySubcategory};
use crate::registry::EntityStatus;
use crate::routes::{
    RegistryTenantQuery, entity_status_refusal, if_match, not_found, parse_entity_status,
    version_mismatch, versioned_created, versioned_ok,
};
use crate::version::UpdateOutcome;

#[utoipa::path(
    get,
    path = "/admin/catalog/display-categories",
    security(("session_cookie" = [])),
    params(
        ("tenant_id" = String, Query, description = "The tenant whose display categories to list (a 26-character ULID)"),
    ),
    responses(
        (status = 200, description = "The tenant's display categories, as an array, each with its `etag`"),
        (status = 400, description = "tenant_id is absent or not a ULID", body = pos_cloud_api::error_schema::ErrorResponse),
        (status = 401, description = "No session", body = pos_cloud_api::error_schema::ErrorResponse),
        (status = 403, description = "The role lacks console.data.read", body = pos_cloud_api::error_schema::ErrorResponse),
        (status = 503, description = "The catalog store is unreachable", body = pos_cloud_api::error_schema::ErrorResponse),
    ),
    tag = "catalog taxonomy",
)]
/// A super-admin lists a tenant's display categories.
pub(super) async fn admin_list_display_categories<Cat, A, C>(
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
    match state.catalog.list_display_categories(tenant_id).await {
        Ok(rows) => (StatusCode::OK, Json(rows)).into_response(),
        Err(error) => catalog_error_response(&error),
    }
}

#[utoipa::path(
    post,
    path = "/admin/catalog/display-categories",
    security(("session_cookie" = [])),
    request_body(
        description = "`tenant_id` and the category's `name`, as the till's layout shows it",
        content_type = "application/json",
    ),
    responses(
        (status = 201, description = "Created, active, with the id minted here. The body is the \
                                      record and the ETag carries its version. Audited"),
        (status = 400, description = "tenant_id is absent or not a ULID", body = pos_cloud_api::error_schema::ErrorResponse),
        (status = 401, description = "No session", body = pos_cloud_api::error_schema::ErrorResponse),
        (status = 403, description = "The role lacks console.catalog.manage", body = pos_cloud_api::error_schema::ErrorResponse),
        (status = 503, description = "The catalog store or the entropy source is unavailable", body = pos_cloud_api::error_schema::ErrorResponse),
    ),
    tag = "catalog taxonomy",
)]
/// A super-admin creates a display category; the id is minted here and returned once.
pub(super) async fn admin_create_display_category<Cat, A, C>(
    State(state): State<CatalogState<Cat, A, C>>,
    headers: HeaderMap,
    Json(request): Json<CreateDisplayCategoryRequest>,
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
    let Some(display_category_id) =
        mint_ulid(state.clock.now().as_milliseconds_since_epoch()).map(DisplayCategoryId::new)
    else {
        return catalog_entropy_unavailable();
    };
    let record = DisplayCategory {
        display_category_id,
        tenant_id,
        name: request.name,
        status: EntityStatus::Active,
    };
    match state.catalog.create_display_category(&record).await {
        Ok(version) => {
            audit_action(
                &state.audit,
                &state.clock,
                &context,
                Some(record.tenant_id),
                "display_category.create",
                "display_category",
                &record.display_category_id.to_string(),
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
    path = "/admin/catalog/display-categories/{display_category_id}",
    security(("session_cookie" = [])),
    params(
        ("display_category_id" = String, Path, description = "The display category (a 26-character ULID)"),
        ("If-Match" = String, Header, description = "The ETag the record was read at (ADR-0094). \
                                                     Required; a wildcard is refused"),
    ),
    request_body(
        description = "`tenant_id`, and the category's `name` and `status` (`active` or `archived`) as they should now stand",
        content_type = "application/json",
    ),
    responses(
        (status = 200, description = "Updated. The body is the record and the ETag carries its new \
                                      version. Audited"),
        (status = 400, description = "display_category_id or tenant_id is not a ULID, status is not `active` or `archived`, or If-Match is \
                                      absent, malformed, or a wildcard", body = pos_cloud_api::error_schema::ErrorResponse),
        (status = 401, description = "No session", body = pos_cloud_api::error_schema::ErrorResponse),
        (status = 403, description = "The role lacks console.catalog.manage", body = pos_cloud_api::error_schema::ErrorResponse),
        (status = 404, description = "The tenant has no display category with that id", body = pos_cloud_api::error_schema::ErrorResponse),
        (status = 412, description = "The display category changed since you read it: re-read it and try \
                                      again", body = pos_cloud_api::error_schema::ErrorResponse),
        (status = 503, description = "The catalog store is unreachable", body = pos_cloud_api::error_schema::ErrorResponse),
    ),
    tag = "catalog taxonomy",
)]
/// A super-admin renames a display category and/or sets its status.
pub(super) async fn admin_update_display_category<Cat, A, C>(
    State(state): State<CatalogState<Cat, A, C>>,
    headers: HeaderMap,
    Path(display_category_id): Path<String>,
    Json(request): Json<UpdateDisplayCategoryRequest>,
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
    let (display_category_id, tenant_id) = match parse_ulid_fields([
        ("display_category_id", &display_category_id),
        ("tenant_id", &request.tenant_id),
    ]) {
        Ok([display_category_id, tenant_id]) => (
            DisplayCategoryId::new(display_category_id),
            TenantId::new(tenant_id),
        ),
        Err(refusal) => return refusal,
    };
    let Some(status) = parse_entity_status(&request.status) else {
        return entity_status_refusal();
    };
    let record = DisplayCategory {
        display_category_id,
        tenant_id,
        name: request.name,
        status,
    };
    let expected = match if_match(&headers) {
        Ok(expected) => expected,
        Err(refusal) => return refusal,
    };
    match state
        .catalog
        .update_display_category(&record, &expected)
        .await
    {
        Ok(UpdateOutcome::Updated(version)) => {
            audit_action(
                &state.audit,
                &state.clock,
                &context,
                Some(record.tenant_id),
                "display_category.update",
                "display_category",
                &record.display_category_id.to_string(),
                None,
                serde_json::to_value(&record).ok(),
            )
            .await;
            versioned_ok(record, &version)
        }
        Ok(UpdateOutcome::VersionMismatch) => version_mismatch(),
        Ok(UpdateOutcome::NotFound) => not_found("display category"),
        Err(error) => catalog_error_response(&error),
    }
}

#[utoipa::path(
    get,
    path = "/admin/catalog/display-subcategories",
    security(("session_cookie" = [])),
    params(
        ("tenant_id" = String, Query, description = "The tenant whose display sub-categories to list (a 26-character ULID)"),
    ),
    responses(
        (status = 200, description = "The tenant's display sub-categories, as an array, each with its `etag`, and the `display_category_id` it belongs to"),
        (status = 400, description = "tenant_id is absent or not a ULID", body = pos_cloud_api::error_schema::ErrorResponse),
        (status = 401, description = "No session", body = pos_cloud_api::error_schema::ErrorResponse),
        (status = 403, description = "The role lacks console.data.read", body = pos_cloud_api::error_schema::ErrorResponse),
        (status = 503, description = "The catalog store is unreachable", body = pos_cloud_api::error_schema::ErrorResponse),
    ),
    tag = "catalog taxonomy",
)]
/// A super-admin lists a tenant's display sub-categories.
pub(super) async fn admin_list_display_subcategories<Cat, A, C>(
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
    match state.catalog.list_display_subcategories(tenant_id).await {
        Ok(rows) => (StatusCode::OK, Json(rows)).into_response(),
        Err(error) => catalog_error_response(&error),
    }
}

#[utoipa::path(
    post,
    path = "/admin/catalog/display-subcategories",
    security(("session_cookie" = [])),
    request_body(
        description = "`tenant_id`, the parent `display_category_id`, and the sub-category's `name`",
        content_type = "application/json",
    ),
    responses(
        (status = 201, description = "Created, active, with the id minted here. The body is the \
                                      record and the ETag carries its version. Audited"),
        (status = 400, description = "tenant_id or display_category_id is absent or not a ULID", body = pos_cloud_api::error_schema::ErrorResponse),
        (status = 401, description = "No session", body = pos_cloud_api::error_schema::ErrorResponse),
        (status = 403, description = "The role lacks console.catalog.manage", body = pos_cloud_api::error_schema::ErrorResponse),
        (status = 503, description = "The catalog store or the entropy source is unavailable", body = pos_cloud_api::error_schema::ErrorResponse),
    ),
    tag = "catalog taxonomy",
)]
/// A super-admin creates a display sub-category under a parent display category; the id is minted here.
pub(super) async fn admin_create_display_subcategory<Cat, A, C>(
    State(state): State<CatalogState<Cat, A, C>>,
    headers: HeaderMap,
    Json(request): Json<CreateDisplaySubcategoryRequest>,
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
    let (tenant_id, display_category_id) = match parse_ulid_fields([
        ("tenant_id", &request.tenant_id),
        ("display_category_id", &request.display_category_id),
    ]) {
        Ok([tenant_id, display_category_id]) => (
            TenantId::new(tenant_id),
            DisplayCategoryId::new(display_category_id),
        ),
        Err(refusal) => return refusal,
    };
    let Some(display_subcategory_id) =
        mint_ulid(state.clock.now().as_milliseconds_since_epoch()).map(DisplaySubcategoryId::new)
    else {
        return catalog_entropy_unavailable();
    };
    let record = DisplaySubcategory {
        display_subcategory_id,
        tenant_id,
        display_category_id,
        name: request.name,
        status: EntityStatus::Active,
    };
    match state.catalog.create_display_subcategory(&record).await {
        Ok(version) => {
            audit_action(
                &state.audit,
                &state.clock,
                &context,
                Some(record.tenant_id),
                "display_subcategory.create",
                "display_subcategory",
                &record.display_subcategory_id.to_string(),
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
    path = "/admin/catalog/display-subcategories/{display_subcategory_id}",
    security(("session_cookie" = [])),
    params(
        ("display_subcategory_id" = String, Path, description = "The display sub-category (a 26-character ULID)"),
        ("If-Match" = String, Header, description = "The ETag the record was read at (ADR-0094). \
                                                     Required; a wildcard is refused"),
    ),
    request_body(
        description = "`tenant_id`, and the sub-category's parent `display_category_id`, `name` and `status` (`active` or `archived`) as they should now stand",
        content_type = "application/json",
    ),
    responses(
        (status = 200, description = "Updated. The body is the record and the ETag carries its new \
                                      version. Audited"),
        (status = 400, description = "display_subcategory_id, tenant_id or display_category_id is not a ULID, status is not `active` or `archived`, or If-Match is \
                                      absent, malformed, or a wildcard", body = pos_cloud_api::error_schema::ErrorResponse),
        (status = 401, description = "No session", body = pos_cloud_api::error_schema::ErrorResponse),
        (status = 403, description = "The role lacks console.catalog.manage", body = pos_cloud_api::error_schema::ErrorResponse),
        (status = 404, description = "The tenant has no display sub-category with that id", body = pos_cloud_api::error_schema::ErrorResponse),
        (status = 412, description = "The display sub-category changed since you read it: re-read it and try \
                                      again", body = pos_cloud_api::error_schema::ErrorResponse),
        (status = 503, description = "The catalog store is unreachable", body = pos_cloud_api::error_schema::ErrorResponse),
    ),
    tag = "catalog taxonomy",
)]
/// A super-admin renames a display sub-category, (re)parents it, and/or sets its status.
pub(super) async fn admin_update_display_subcategory<Cat, A, C>(
    State(state): State<CatalogState<Cat, A, C>>,
    headers: HeaderMap,
    Path(display_subcategory_id): Path<String>,
    Json(request): Json<UpdateDisplaySubcategoryRequest>,
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
    let (display_subcategory_id, tenant_id, display_category_id) = match parse_ulid_fields([
        ("display_subcategory_id", &display_subcategory_id),
        ("tenant_id", &request.tenant_id),
        ("display_category_id", &request.display_category_id),
    ]) {
        Ok([display_subcategory_id, tenant_id, display_category_id]) => (
            DisplaySubcategoryId::new(display_subcategory_id),
            TenantId::new(tenant_id),
            DisplayCategoryId::new(display_category_id),
        ),
        Err(refusal) => return refusal,
    };
    let Some(status) = parse_entity_status(&request.status) else {
        return entity_status_refusal();
    };
    let record = DisplaySubcategory {
        display_subcategory_id,
        tenant_id,
        display_category_id,
        name: request.name,
        status,
    };
    let expected = match if_match(&headers) {
        Ok(expected) => expected,
        Err(refusal) => return refusal,
    };
    match state
        .catalog
        .update_display_subcategory(&record, &expected)
        .await
    {
        Ok(UpdateOutcome::Updated(version)) => {
            audit_action(
                &state.audit,
                &state.clock,
                &context,
                Some(record.tenant_id),
                "display_subcategory.update",
                "display_subcategory",
                &record.display_subcategory_id.to_string(),
                None,
                serde_json::to_value(&record).ok(),
            )
            .await;
            versioned_ok(record, &version)
        }
        Ok(UpdateOutcome::VersionMismatch) => version_mismatch(),
        Ok(UpdateOutcome::NotFound) => not_found("display sub-category"),
        Err(error) => catalog_error_response(&error),
    }
}
