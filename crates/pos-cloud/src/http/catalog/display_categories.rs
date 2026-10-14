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
use crate::http::{
    RegistryTenantQuery, entity_status_refusal, if_match, not_found, parse_entity_status,
    version_mismatch, versioned_created, versioned_ok,
};
use crate::registry::EntityStatus;
use crate::version::UpdateOutcome;

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
