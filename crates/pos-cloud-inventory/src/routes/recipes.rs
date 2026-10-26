// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The `/admin/inventory/recipes` handlers: list, read, create, update and delete.

use axum::Json;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use pos_cloud_api::console::{audit_action, require_permission};
use pos_cloud_api::response::parse_ulid_fields;
use pos_proto::determinism::ClockSource;
use pos_proto::ids::{MenuItemId, TenantId};

use super::{
    CreateRecipeRequest, InventoryState, RecipeAuditSummary, RecipeRequest, build_recipe,
    inventory_error_response,
};
use crate::auth::admin::AdminStore;
use crate::auth::console_rbac::ConsolePermission;
use crate::inventory::InventoryStore;
use crate::version::{CreateOutcome, UpdateOutcome, Versioned};
use pos_cloud_api::conditional::{
    already_exists, if_match, version_mismatch, versioned_created, versioned_ok,
};
use pos_cloud_api::refusal::not_found;
use pos_cloud_config::route::RegistryTenantQuery;
use pos_proto::inventory::PublishedRecipe;

#[utoipa::path(
    get,
    path = "/admin/inventory/recipes",
    extensions(("x-permission" = json!("console.data.read"))),
    security(("session_cookie" = [])),
    params(
        ("tenant_id" = String, Query, description = "The tenant (a 26-character ULID)"),
    ),
    responses(
        (status = 200, body = [Versioned<PublishedRecipe>], description = "The tenant's recipes, as an array, each with its `etag`"),
        (status = 400, description = "tenant_id is absent or not a ULID", body = pos_cloud_api::error_schema::ErrorResponse),
        (status = 401, description = "No session", body = pos_cloud_api::error_schema::ErrorResponse),
        (status = 403, description = "The role lacks console.data.read", body = pos_cloud_api::error_schema::ErrorResponse),
        (status = 503, description = "The inventory store is unreachable", body = pos_cloud_api::error_schema::ErrorResponse),
    ),
    tag = "inventory",
)]
/// A super-admin lists a tenant's authored recipes.
pub(super) async fn admin_list_recipes<Inv, A, C>(
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
    match state.inventory.list_recipes(tenant_id).await {
        Ok(recipes) => (StatusCode::OK, Json(recipes)).into_response(),
        Err(error) => inventory_error_response(&error),
    }
}

#[utoipa::path(
    get,
    path = "/admin/inventory/recipes/{item_id}",
    extensions(("x-permission" = json!("console.data.read"))),
    security(("session_cookie" = [])),
    params(
        ("item_id" = String, Path, description = "The menu item or modifier the recipe makes (a 26-character ULID)"),
        ("tenant_id" = String, Query, description = "The tenant (a 26-character ULID)"),
    ),
    responses(
        (status = 200, body = Versioned<PublishedRecipe>, description = "The recipe. The ETag carries the version it was read at"),
        (status = 400, description = "item_id or tenant_id is absent or not a ULID", body = pos_cloud_api::error_schema::ErrorResponse),
        (status = 401, description = "No session", body = pos_cloud_api::error_schema::ErrorResponse),
        (status = 403, description = "The role lacks console.data.read", body = pos_cloud_api::error_schema::ErrorResponse),
        (status = 404, description = "The tenant has no recipe for that item", body = pos_cloud_api::error_schema::ErrorResponse),
        (status = 503, description = "The inventory store is unreachable", body = pos_cloud_api::error_schema::ErrorResponse),
    ),
    tag = "inventory",
)]
/// A super-admin reads one recipe by the item it makes.
pub(super) async fn admin_get_recipe<Inv, A, C>(
    State(state): State<InventoryState<Inv, A, C>>,
    headers: HeaderMap,
    Path(item_id): Path<String>,
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
    let (tenant_id, item) =
        match parse_ulid_fields([("tenant_id", &query.tenant_id), ("item_id", &item_id)]) {
            Ok([tenant_id, item]) => (TenantId::new(tenant_id), MenuItemId::new(item)),
            Err(refusal) => return refusal,
        };
    match state.inventory.get_recipe(tenant_id, item).await {
        Ok(Some(row)) => versioned_ok(row.record, &row.etag),
        Ok(None) => not_found("recipe"),
        Err(error) => inventory_error_response(&error),
    }
}

#[utoipa::path(
    post,
    path = "/admin/inventory/recipes",
    extensions(("x-permission" = json!("console.inventory.manage"))),
    security(("session_cookie" = [])),
    request_body(
        content = CreateRecipeRequest, description = "`tenant_id` and `item_id`, the menu item or modifier the recipe makes, and \
                       `lines`, the bill of materials: each line an `ingredient` and `per_unit`, the \
                       amount one unit consumes in thousandths of the ingredient's unit, which must \
                       be positive; and `auto_86_threshold`, which may not be negative",
        content_type = "application/json",
    ),
    responses(
        (status = 201, body = Versioned<PublishedRecipe>, description = "Created. The body is the recipe and the ETag carries its version. Audited \
                                      with the item, its line count and its threshold, never the amounts"),
        (status = 400, description = "tenant_id or item_id is absent or not a ULID, auto_86_threshold is negative, or a line consumes nothing", body = pos_cloud_api::error_schema::ErrorResponse),
        (status = 401, description = "No session", body = pos_cloud_api::error_schema::ErrorResponse),
        (status = 403, description = "The role lacks console.inventory.manage", body = pos_cloud_api::error_schema::ErrorResponse),
        (status = 409, description = "The item already has a recipe: edit that one instead", body = pos_cloud_api::error_schema::ErrorResponse),
        (status = 503, description = "The inventory store is unreachable", body = pos_cloud_api::error_schema::ErrorResponse),
    ),
    tag = "inventory",
)]
/// A super-admin creates the recipe for an item, refusing if that item already has one.
///
/// The route this replaced was a single `PUT` upsert that re-derived create-versus-update at
/// runtime: it listed the tenant's recipes to pick between `201`/`200` and between the
/// `create`/`update` audit action, then wrote unconditionally. That read raced its own write — a
/// concurrent create or delete in between made *both* the status code and the audit entry wrong —
/// and a caller asking to add a recipe silently replaced the bill of materials of one already
/// there. [ADR-0095](../../../docs/adr/0095-conditional-writes-for-collections.md) §(ii).
pub(super) async fn admin_create_recipe<Inv, A, C>(
    State(state): State<InventoryState<Inv, A, C>>,
    headers: HeaderMap,
    Json(request): Json<CreateRecipeRequest>,
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
    let (tenant_id, item) = match parse_ulid_fields([
        ("tenant_id", &request.tenant_id),
        ("item_id", &request.item_id),
    ]) {
        Ok([tenant_id, item]) => (TenantId::new(tenant_id), MenuItemId::new(item)),
        Err(refusal) => return refusal,
    };
    let recipe = match build_recipe(&request.into_write(), item) {
        Ok(recipe) => recipe,
        Err(refusal) => return refusal.into_response(),
    };
    match state.inventory.create_recipe(tenant_id, &recipe).await {
        Ok(CreateOutcome::Created(version)) => {
            audit_action(
                &state.audit,
                &state.clock,
                &context,
                Some(tenant_id),
                "inventory.recipe.create",
                "recipe",
                &item.to_string(),
                None,
                serde_json::to_value(RecipeAuditSummary::of(&recipe)).ok(),
            )
            .await;
            versioned_created(recipe, &version)
        }
        Ok(CreateOutcome::AlreadyExists) => already_exists("recipe"),
        Err(error) => inventory_error_response(&error),
    }
}

#[utoipa::path(
    put,
    path = "/admin/inventory/recipes/{item_id}",
    extensions(("x-permission" = json!("console.inventory.manage"))),
    security(("session_cookie" = [])),
    params(
        ("item_id" = String, Path, description = "The menu item or modifier the recipe makes (a 26-character ULID)"),
        ("If-Match" = String, Header, description = "The ETag the record was read at (ADR-0094). \
                                                     Required; a wildcard is refused"),
    ),
    request_body(
        content = RecipeRequest, description = "`tenant_id`, and `lines`, the bill of materials: each line an `ingredient` and `per_unit`, the \
                       amount one unit consumes in thousandths of the ingredient's unit, which must \
                       be positive; and `auto_86_threshold`, which may not be negative, as the recipe should now stand",
        content_type = "application/json",
    ),
    responses(
        (status = 200, body = Versioned<PublishedRecipe>, description = "Updated. The body is the recipe and the ETag carries its new version. Audited \
                                      without the amounts"),
        (status = 400, description = "item_id or tenant_id is not a ULID, auto_86_threshold is negative, or a line consumes nothing, or If-Match is absent, \
                                      malformed or a wildcard", body = pos_cloud_api::error_schema::ErrorResponse),
        (status = 401, description = "No session", body = pos_cloud_api::error_schema::ErrorResponse),
        (status = 403, description = "The role lacks console.inventory.manage", body = pos_cloud_api::error_schema::ErrorResponse),
        (status = 404, description = "The tenant has no recipe for that item", body = pos_cloud_api::error_schema::ErrorResponse),
        (status = 412, description = "The recipe changed since you read it: re-read it and try again", body = pos_cloud_api::error_schema::ErrorResponse),
        (status = 503, description = "The inventory store is unreachable", body = pos_cloud_api::error_schema::ErrorResponse),
    ),
    tag = "inventory",
)]
/// A super-admin replaces the recipe for the path item, only at the version they read.
pub(super) async fn admin_update_recipe<Inv, A, C>(
    State(state): State<InventoryState<Inv, A, C>>,
    headers: HeaderMap,
    Path(item_id): Path<String>,
    Json(request): Json<RecipeRequest>,
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
    let (tenant_id, item) =
        match parse_ulid_fields([("tenant_id", &request.tenant_id), ("item_id", &item_id)]) {
            Ok([tenant_id, item]) => (TenantId::new(tenant_id), MenuItemId::new(item)),
            Err(refusal) => return refusal,
        };
    // Read for the audit's "before" only. It no longer decides the status code or the action name:
    // the version comparison in the write does that, so a race here can no longer mislabel either.
    let before = match state.inventory.get_recipe(tenant_id, item).await {
        Ok(row) => row.map(|row| row.record),
        Err(error) => return inventory_error_response(&error),
    };
    let recipe = match build_recipe(&request, item) {
        Ok(recipe) => recipe,
        Err(refusal) => return refusal.into_response(),
    };
    let expected = match if_match(&headers) {
        Ok(expected) => expected,
        Err(refusal) => return refusal,
    };
    match state
        .inventory
        .update_recipe(tenant_id, &recipe, &expected)
        .await
    {
        Ok(UpdateOutcome::Updated(version)) => {
            audit_action(
                &state.audit,
                &state.clock,
                &context,
                Some(tenant_id),
                "inventory.recipe.update",
                "recipe",
                &item.to_string(),
                before
                    .as_ref()
                    .and_then(|r| serde_json::to_value(RecipeAuditSummary::of(r)).ok()),
                serde_json::to_value(RecipeAuditSummary::of(&recipe)).ok(),
            )
            .await;
            versioned_ok(recipe, &version)
        }
        Ok(UpdateOutcome::VersionMismatch) => version_mismatch(),
        Ok(UpdateOutcome::NotFound) => not_found("recipe"),
        Err(error) => inventory_error_response(&error),
    }
}

#[utoipa::path(
    delete,
    path = "/admin/inventory/recipes/{item_id}",
    extensions(("x-permission" = json!("console.inventory.manage"))),
    security(("session_cookie" = [])),
    params(
        ("item_id" = String, Path, description = "The menu item or modifier the recipe makes (a 26-character ULID)"),
        ("tenant_id" = String, Query, description = "The tenant (a 26-character ULID)"),
    ),
    responses(
        (status = 204, description = "Deleted. Audited"),
        (status = 400, description = "item_id or tenant_id is absent or not a ULID", body = pos_cloud_api::error_schema::ErrorResponse),
        (status = 401, description = "No session", body = pos_cloud_api::error_schema::ErrorResponse),
        (status = 403, description = "The role lacks console.inventory.manage", body = pos_cloud_api::error_schema::ErrorResponse),
        (status = 404, description = "The tenant has no recipe for that item", body = pos_cloud_api::error_schema::ErrorResponse),
        (status = 503, description = "The inventory store is unreachable", body = pos_cloud_api::error_schema::ErrorResponse),
    ),
    tag = "inventory",
)]
/// A super-admin deletes a recipe by the item it makes.
pub(super) async fn admin_delete_recipe<Inv, A, C>(
    State(state): State<InventoryState<Inv, A, C>>,
    headers: HeaderMap,
    Path(item_id): Path<String>,
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
    let (tenant_id, item) =
        match parse_ulid_fields([("tenant_id", &query.tenant_id), ("item_id", &item_id)]) {
            Ok([tenant_id, item]) => (TenantId::new(tenant_id), MenuItemId::new(item)),
            Err(refusal) => return refusal,
        };
    let before = match state.inventory.get_recipe(tenant_id, item).await {
        Ok(row) => row.map(|row| row.record),
        Err(error) => return inventory_error_response(&error),
    };
    let Some(before) = before else {
        return not_found("recipe");
    };
    match state.inventory.delete_recipe(tenant_id, item).await {
        Ok(()) => {
            audit_action(
                &state.audit,
                &state.clock,
                &context,
                Some(tenant_id),
                "inventory.recipe.delete",
                "recipe",
                &item.to_string(),
                serde_json::to_value(RecipeAuditSummary::of(&before)).ok(),
                None,
            )
            .await;
            StatusCode::NO_CONTENT.into_response()
        }
        Err(error) => inventory_error_response(&error),
    }
}
