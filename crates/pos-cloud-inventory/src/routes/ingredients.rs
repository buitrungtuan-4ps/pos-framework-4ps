// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The `/admin/inventory/ingredients` handlers: list, read, create, update and delete.

use axum::Json;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use pos_cloud_api::console::{audit_action, mint_ulid, require_permission};
use pos_cloud_api::response::parse_ulid_fields;
use pos_proto::determinism::ClockSource;
use pos_proto::ids::{IngredientId, TenantId};

use super::{
    IngredientAuditSummary, IngredientRequest, InventoryState, build_ingredient,
    inventory_entropy_unavailable, inventory_error_response,
};
use crate::auth::admin::AdminStore;
use crate::auth::console_rbac::ConsolePermission;
use crate::inventory::InventoryStore;
use crate::version::{CreateOutcome, UpdateOutcome};
use pos_cloud_api::conditional::{
    already_exists, if_match, version_mismatch, versioned_created, versioned_ok,
};
use pos_cloud_api::refusal::not_found;
use pos_cloud_config::route::RegistryTenantQuery;

/// A super-admin lists a tenant's authored ingredients.
pub(super) async fn admin_list_ingredients<Inv, A, C>(
    State(state): State<InventoryState<Inv, A, C>>,
    headers: HeaderMap,
    Query(query): Query<RegistryTenantQuery>,
) -> Response
where
    Inv: InventoryStore + Clone + Send + Sync + 'static,
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
    match state.inventory.list_ingredients(tenant_id).await {
        Ok(ingredients) => (StatusCode::OK, Json(ingredients)).into_response(),
        Err(error) => inventory_error_response(&error),
    }
}

/// A super-admin reads one ingredient by id.
pub(super) async fn admin_get_ingredient<Inv, A, C>(
    State(state): State<InventoryState<Inv, A, C>>,
    headers: HeaderMap,
    Path(ingredient_id): Path<String>,
    Query(query): Query<RegistryTenantQuery>,
) -> Response
where
    Inv: InventoryStore + Clone + Send + Sync + 'static,
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
    let (tenant_id, ingredient_id) = match parse_ulid_fields([
        ("tenant_id", &query.tenant_id),
        ("ingredient_id", &ingredient_id),
    ]) {
        Ok([tenant_id, ingredient_id]) => {
            (TenantId::new(tenant_id), IngredientId::new(ingredient_id))
        }
        Err(refusal) => return refusal,
    };
    match state
        .inventory
        .get_ingredient(tenant_id, ingredient_id)
        .await
    {
        Ok(Some(row)) => versioned_ok(row.record, &row.etag),
        Ok(None) => not_found("ingredient"),
        Err(error) => inventory_error_response(&error),
    }
}

/// A super-admin creates an ingredient; the server mints its id.
pub(super) async fn admin_create_ingredient<Inv, A, C>(
    State(state): State<InventoryState<Inv, A, C>>,
    headers: HeaderMap,
    Json(request): Json<IngredientRequest>,
) -> Response
where
    Inv: InventoryStore + Clone + Send + Sync + 'static,
    A: AdminStore + Clone + Send + Sync + 'static,
    C: ClockSource + Clone + Send + Sync + 'static,
{
    let context = match require_permission(
        &state.admin,
        &state.clock,
        &headers,
        ConsolePermission::ManageInventory,
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
    let Some(ingredient_id) =
        mint_ulid(state.clock.now().as_milliseconds_since_epoch()).map(IngredientId::new)
    else {
        return inventory_entropy_unavailable();
    };
    let ingredient = match build_ingredient(&request, ingredient_id) {
        Ok(ingredient) => ingredient,
        Err(refusal) => return refusal.into_response(),
    };
    match state
        .inventory
        .create_ingredient(tenant_id, &ingredient)
        .await
    {
        Ok(CreateOutcome::Created(version)) => {
            audit_action(
                &state.audit,
                &state.clock,
                &context,
                Some(tenant_id),
                "inventory.ingredient.create",
                "ingredient",
                &ingredient_id.to_string(),
                None,
                serde_json::to_value(IngredientAuditSummary::of(&ingredient)).ok(),
            )
            .await;
            versioned_created(ingredient, &version)
        }
        // Unreachable while the id is minted here rather than sent by the caller, and answered
        // anyway — the seam cannot know that, and silently overwriting is what this slice removed.
        Ok(CreateOutcome::AlreadyExists) => already_exists("ingredient"),
        Err(error) => inventory_error_response(&error),
    }
}

/// A super-admin updates an ingredient in place, by the path id.
pub(super) async fn admin_update_ingredient<Inv, A, C>(
    State(state): State<InventoryState<Inv, A, C>>,
    headers: HeaderMap,
    Path(ingredient_id): Path<String>,
    Json(request): Json<IngredientRequest>,
) -> Response
where
    Inv: InventoryStore + Clone + Send + Sync + 'static,
    A: AdminStore + Clone + Send + Sync + 'static,
    C: ClockSource + Clone + Send + Sync + 'static,
{
    let context = match require_permission(
        &state.admin,
        &state.clock,
        &headers,
        ConsolePermission::ManageInventory,
    )
    .await
    {
        Ok(context) => context,
        Err(denied) => return denied,
    };
    let (tenant_id, ingredient_id) = match parse_ulid_fields([
        ("tenant_id", &request.tenant_id),
        ("ingredient_id", &ingredient_id),
    ]) {
        Ok([tenant_id, ingredient_id]) => {
            (TenantId::new(tenant_id), IngredientId::new(ingredient_id))
        }
        Err(refusal) => return refusal,
    };
    let before = match state
        .inventory
        .get_ingredient(tenant_id, ingredient_id)
        .await
    {
        Ok(row) => row.map(|row| row.record),
        Err(error) => return inventory_error_response(&error),
    };
    let Some(before) = before else {
        return not_found("ingredient");
    };
    let ingredient = match build_ingredient(&request, ingredient_id) {
        Ok(ingredient) => ingredient,
        Err(refusal) => return refusal.into_response(),
    };
    let expected = match if_match(&headers) {
        Ok(expected) => expected,
        Err(refusal) => return refusal,
    };
    match state
        .inventory
        .update_ingredient(tenant_id, &ingredient, &expected)
        .await
    {
        Ok(UpdateOutcome::Updated(version)) => {
            audit_action(
                &state.audit,
                &state.clock,
                &context,
                Some(tenant_id),
                "inventory.ingredient.update",
                "ingredient",
                &ingredient_id.to_string(),
                serde_json::to_value(IngredientAuditSummary::of(&before)).ok(),
                serde_json::to_value(IngredientAuditSummary::of(&ingredient)).ok(),
            )
            .await;
            versioned_ok(ingredient, &version)
        }
        Ok(UpdateOutcome::VersionMismatch) => version_mismatch(),
        Ok(UpdateOutcome::NotFound) => not_found("ingredient"),
        Err(error) => inventory_error_response(&error),
    }
}

/// A super-admin deletes an ingredient by id.
pub(super) async fn admin_delete_ingredient<Inv, A, C>(
    State(state): State<InventoryState<Inv, A, C>>,
    headers: HeaderMap,
    Path(ingredient_id): Path<String>,
    Query(query): Query<RegistryTenantQuery>,
) -> Response
where
    Inv: InventoryStore + Clone + Send + Sync + 'static,
    A: AdminStore + Clone + Send + Sync + 'static,
    C: ClockSource + Clone + Send + Sync + 'static,
{
    let context = match require_permission(
        &state.admin,
        &state.clock,
        &headers,
        ConsolePermission::ManageInventory,
    )
    .await
    {
        Ok(context) => context,
        Err(denied) => return denied,
    };
    let (tenant_id, ingredient_id) = match parse_ulid_fields([
        ("tenant_id", &query.tenant_id),
        ("ingredient_id", &ingredient_id),
    ]) {
        Ok([tenant_id, ingredient_id]) => {
            (TenantId::new(tenant_id), IngredientId::new(ingredient_id))
        }
        Err(refusal) => return refusal,
    };
    let before = match state
        .inventory
        .get_ingredient(tenant_id, ingredient_id)
        .await
    {
        Ok(row) => row.map(|row| row.record),
        Err(error) => return inventory_error_response(&error),
    };
    let Some(before) = before else {
        return not_found("ingredient");
    };
    match state
        .inventory
        .delete_ingredient(tenant_id, ingredient_id)
        .await
    {
        Ok(()) => {
            audit_action(
                &state.audit,
                &state.clock,
                &context,
                Some(tenant_id),
                "inventory.ingredient.delete",
                "ingredient",
                &ingredient_id.to_string(),
                serde_json::to_value(IngredientAuditSummary::of(&before)).ok(),
                None,
            )
            .await;
            StatusCode::NO_CONTENT.into_response()
        }
        Err(error) => inventory_error_response(&error),
    }
}
