// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! `/admin/catalog/item-categories` and `/admin/catalog/item-subcategories`.

use axum::Json;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use pos_cloud_api::console::{audit_action, mint_ulid, require_permission};
use pos_cloud_api::response::parse_ulid_fields;
use pos_proto::determinism::ClockSource;
use pos_proto::ids::TenantId;

use super::requests::{
    CreateItemCategoryRequest, CreateItemSubcategoryRequest, UpdateItemCategoryRequest,
    UpdateItemSubcategoryRequest,
};
use super::tax_classes::{active_items_referencing, still_referenced};
use super::{CatalogState, catalog_entropy_unavailable, catalog_error_response};
use crate::auth::admin::AdminStore;
use crate::auth::console_rbac::ConsolePermission;
use crate::catalog::{
    CatalogStore, ItemCategory, ItemCategoryId, ItemSubcategory, ItemSubcategoryId,
};
use crate::registry::EntityStatus;
use crate::routes::{
    RegistryTenantQuery, entity_status_refusal, if_match, not_found, parse_entity_status,
    version_mismatch, versioned_created, versioned_ok,
};
use crate::version::UpdateOutcome;

/// A super-admin lists a tenant's item categories.
pub(super) async fn admin_list_item_categories<Cat, A, C>(
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
    match state.catalog.list_item_categories(tenant_id).await {
        Ok(rows) => (StatusCode::OK, Json(rows)).into_response(),
        Err(error) => catalog_error_response(&error),
    }
}

/// A super-admin creates an item category; the id is minted here and returned once.
pub(super) async fn admin_create_item_category<Cat, A, C>(
    State(state): State<CatalogState<Cat, A, C>>,
    headers: HeaderMap,
    Json(request): Json<CreateItemCategoryRequest>,
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
    let Some(item_category_id) =
        mint_ulid(state.clock.now().as_milliseconds_since_epoch()).map(ItemCategoryId::new)
    else {
        return catalog_entropy_unavailable();
    };
    let record = ItemCategory {
        item_category_id,
        tenant_id,
        name: request.name,
        status: EntityStatus::Active,
    };
    match state.catalog.create_item_category(&record).await {
        Ok(version) => {
            audit_action(
                &state.audit,
                &state.clock,
                &context,
                Some(record.tenant_id),
                "item_category.create",
                "item_category",
                &record.item_category_id.to_string(),
                None,
                serde_json::to_value(&record).ok(),
            )
            .await;
            versioned_created(record, &version)
        }
        Err(error) => catalog_error_response(&error),
    }
}

/// A super-admin renames an item category and/or sets its status.
pub(super) async fn admin_update_item_category<Cat, A, C>(
    State(state): State<CatalogState<Cat, A, C>>,
    headers: HeaderMap,
    Path(item_category_id): Path<String>,
    Json(request): Json<UpdateItemCategoryRequest>,
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
    let (item_category_id, tenant_id) = match parse_ulid_fields([
        ("item_category_id", &item_category_id),
        ("tenant_id", &request.tenant_id),
    ]) {
        Ok([item_category_id, tenant_id]) => (
            ItemCategoryId::new(item_category_id),
            TenantId::new(tenant_id),
        ),
        Err(refusal) => return refusal,
    };
    let Some(status) = parse_entity_status(&request.status) else {
        return entity_status_refusal();
    };
    let record = ItemCategory {
        item_category_id,
        tenant_id,
        name: request.name,
        status,
    };
    // Refuse the archive while active items still point here (see `active_items_referencing`).
    if status == EntityStatus::Archived {
        match active_items_referencing(&state.catalog, tenant_id, |item| {
            item.item_category_id == Some(item_category_id)
        })
        .await
        {
            Ok(0) => {}
            Ok(count) => return still_referenced("category", count),
            Err(error) => return catalog_error_response(&error),
        }
    }
    let expected = match if_match(&headers) {
        Ok(expected) => expected,
        Err(refusal) => return refusal,
    };
    match state.catalog.update_item_category(&record, &expected).await {
        Ok(UpdateOutcome::Updated(version)) => {
            audit_action(
                &state.audit,
                &state.clock,
                &context,
                Some(record.tenant_id),
                "item_category.update",
                "item_category",
                &record.item_category_id.to_string(),
                None,
                serde_json::to_value(&record).ok(),
            )
            .await;
            versioned_ok(record, &version)
        }
        Ok(UpdateOutcome::VersionMismatch) => version_mismatch(),
        Ok(UpdateOutcome::NotFound) => not_found("item category"),
        Err(error) => catalog_error_response(&error),
    }
}

/// A super-admin lists a tenant's item sub-categories.
pub(super) async fn admin_list_item_subcategories<Cat, A, C>(
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
    match state.catalog.list_item_subcategories(tenant_id).await {
        Ok(rows) => (StatusCode::OK, Json(rows)).into_response(),
        Err(error) => catalog_error_response(&error),
    }
}

/// A super-admin creates an item sub-category under a parent category; the id is minted here.
pub(super) async fn admin_create_item_subcategory<Cat, A, C>(
    State(state): State<CatalogState<Cat, A, C>>,
    headers: HeaderMap,
    Json(request): Json<CreateItemSubcategoryRequest>,
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
    let (tenant_id, item_category_id) = match parse_ulid_fields([
        ("tenant_id", &request.tenant_id),
        ("item_category_id", &request.item_category_id),
    ]) {
        Ok([tenant_id, item_category_id]) => (
            TenantId::new(tenant_id),
            ItemCategoryId::new(item_category_id),
        ),
        Err(refusal) => return refusal,
    };
    let Some(item_subcategory_id) =
        mint_ulid(state.clock.now().as_milliseconds_since_epoch()).map(ItemSubcategoryId::new)
    else {
        return catalog_entropy_unavailable();
    };
    let record = ItemSubcategory {
        item_subcategory_id,
        tenant_id,
        item_category_id,
        name: request.name,
        status: EntityStatus::Active,
    };
    match state.catalog.create_item_subcategory(&record).await {
        Ok(version) => {
            audit_action(
                &state.audit,
                &state.clock,
                &context,
                Some(record.tenant_id),
                "item_subcategory.create",
                "item_subcategory",
                &record.item_subcategory_id.to_string(),
                None,
                serde_json::to_value(&record).ok(),
            )
            .await;
            versioned_created(record, &version)
        }
        Err(error) => catalog_error_response(&error),
    }
}

/// A super-admin renames an item sub-category, (re)parents it, and/or sets its status.
pub(super) async fn admin_update_item_subcategory<Cat, A, C>(
    State(state): State<CatalogState<Cat, A, C>>,
    headers: HeaderMap,
    Path(item_subcategory_id): Path<String>,
    Json(request): Json<UpdateItemSubcategoryRequest>,
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
    let (item_subcategory_id, tenant_id, item_category_id) = match parse_ulid_fields([
        ("item_subcategory_id", &item_subcategory_id),
        ("tenant_id", &request.tenant_id),
        ("item_category_id", &request.item_category_id),
    ]) {
        Ok([item_subcategory_id, tenant_id, item_category_id]) => (
            ItemSubcategoryId::new(item_subcategory_id),
            TenantId::new(tenant_id),
            ItemCategoryId::new(item_category_id),
        ),
        Err(refusal) => return refusal,
    };
    let Some(status) = parse_entity_status(&request.status) else {
        return entity_status_refusal();
    };
    let record = ItemSubcategory {
        item_subcategory_id,
        tenant_id,
        item_category_id,
        name: request.name,
        status,
    };
    // Refuse the archive while active items still point here (see `active_items_referencing`).
    if status == EntityStatus::Archived {
        match active_items_referencing(&state.catalog, tenant_id, |item| {
            item.item_subcategory_id == Some(item_subcategory_id)
        })
        .await
        {
            Ok(0) => {}
            Ok(count) => return still_referenced("sub-category", count),
            Err(error) => return catalog_error_response(&error),
        }
    }
    let expected = match if_match(&headers) {
        Ok(expected) => expected,
        Err(refusal) => return refusal,
    };
    match state
        .catalog
        .update_item_subcategory(&record, &expected)
        .await
    {
        Ok(UpdateOutcome::Updated(version)) => {
            audit_action(
                &state.audit,
                &state.clock,
                &context,
                Some(record.tenant_id),
                "item_subcategory.update",
                "item_subcategory",
                &record.item_subcategory_id.to_string(),
                None,
                serde_json::to_value(&record).ok(),
            )
            .await;
            versioned_ok(record, &version)
        }
        Ok(UpdateOutcome::VersionMismatch) => version_mismatch(),
        Ok(UpdateOutcome::NotFound) => not_found("item sub-category"),
        Err(error) => catalog_error_response(&error),
    }
}
