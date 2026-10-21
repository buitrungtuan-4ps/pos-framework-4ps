// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! `/admin/catalog/modifier-groups`: list, create and update a tenant's modifier groups.

use axum::Json;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use pos_cloud_api::console::{audit_action, mint_ulid, require_permission};
use pos_cloud_api::response::{parse_ulid_fields, ulid_refusal};
use pos_proto::determinism::ClockSource;
use pos_proto::ids::TenantId;

use super::items::parse_item_id_list;
use super::requests::{CreateModifierGroupRequest, UpdateModifierGroupRequest};
use super::{CatalogState, catalog_entropy_unavailable, catalog_error_response};
use crate::auth::admin::AdminStore;
use crate::auth::console_rbac::ConsolePermission;
use crate::catalog::{CatalogStore, ModifierGroup, ModifierGroupId};
use crate::registry::EntityStatus;
use crate::routes::{
    RegistryTenantQuery, entity_status_refusal, if_match, not_found, parse_entity_status,
    version_mismatch, versioned_created, versioned_ok,
};
use crate::version::UpdateOutcome;

#[utoipa::path(
    get,
    path = "/admin/catalog/modifier-groups",
    extensions(("x-permission" = json!("console.data.read"))),
    security(("session_cookie" = [])),
    params(
        ("tenant_id" = String, Query, description = "The tenant (a 26-character ULID)"),
    ),
    responses(
        (status = 200, description = "The tenant's modifier groups, as an array, each with its \
                                      `etag`. A group is tenant-wide; a publish keeps the ones the \
                                      menu's channels can offer"),
        (status = 400, description = "tenant_id is absent or not a ULID", body = pos_cloud_api::error_schema::ErrorResponse),
        (status = 401, description = "No session", body = pos_cloud_api::error_schema::ErrorResponse),
        (status = 403, description = "The role lacks console.data.read", body = pos_cloud_api::error_schema::ErrorResponse),
        (status = 503, description = "The catalog store is unreachable", body = pos_cloud_api::error_schema::ErrorResponse),
    ),
    tag = "modifiers",
)]
/// A super-admin lists a tenant's modifier groups.
pub(super) async fn admin_list_modifier_groups<Cat, A, C>(
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
    match state.catalog.list_modifier_groups(tenant_id).await {
        Ok(rows) => (StatusCode::OK, Json(rows)).into_response(),
        Err(error) => catalog_error_response(&error),
    }
}

#[utoipa::path(
    post,
    path = "/admin/catalog/modifier-groups",
    extensions(("x-permission" = json!("console.catalog.manage"))),
    security(("session_cookie" = [])),
    request_body(
        description = "`tenant_id`, then `name`, `min_select` and `max_select` (how many of the group a guest must and may pick), \
                       `member_item_ids` (the items it offers) and `attached_item_ids` (the items it \
                       is offered on)",
        content_type = "application/json",
    ),
    responses(
        (status = 201, description = "Created, active, with the id minted here. The body is the \
                                      record and the ETag carries its version. Audited"),
        (status = 400, description = "tenant_id or an item id is not a ULID", body = pos_cloud_api::error_schema::ErrorResponse),
        (status = 401, description = "No session", body = pos_cloud_api::error_schema::ErrorResponse),
        (status = 403, description = "The role lacks console.catalog.manage", body = pos_cloud_api::error_schema::ErrorResponse),
        (status = 503, description = "The catalog store or the entropy source is unavailable", body = pos_cloud_api::error_schema::ErrorResponse),
    ),
    tag = "modifiers",
)]
/// A super-admin creates a modifier group; the id is minted here and returned once.
pub(super) async fn admin_create_modifier_group<Cat, A, C>(
    State(state): State<CatalogState<Cat, A, C>>,
    headers: HeaderMap,
    Json(request): Json<CreateModifierGroupRequest>,
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
    let Ok(member_item_ids) = parse_item_id_list(&request.member_item_ids) else {
        return ulid_refusal(&["member_item_ids"]);
    };
    let Ok(attached_item_ids) = parse_item_id_list(&request.attached_item_ids) else {
        return ulid_refusal(&["attached_item_ids"]);
    };
    let Some(modifier_group_id) =
        mint_ulid(state.clock.now().as_milliseconds_since_epoch()).map(ModifierGroupId::new)
    else {
        return catalog_entropy_unavailable();
    };
    let record = ModifierGroup {
        modifier_group_id,
        tenant_id,
        name: request.name,
        min_select: request.min_select,
        max_select: request.max_select,
        member_item_ids,
        attached_item_ids,
        status: EntityStatus::Active,
    };
    match state.catalog.create_modifier_group(&record).await {
        Ok(version) => {
            audit_action(
                &state.audit,
                &state.clock,
                &context,
                Some(record.tenant_id),
                "modifier_group.create",
                "modifier_group",
                &record.modifier_group_id.to_string(),
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
    path = "/admin/catalog/modifier-groups/{modifier_group_id}",
    extensions(("x-permission" = json!("console.catalog.manage"))),
    security(("session_cookie" = [])),
    params(
        ("modifier_group_id" = String, Path, description = "The group (a 26-character ULID)"),
        ("If-Match" = String, Header, description = "The ETag the record was read at (ADR-0094). \
                                                     Required; a wildcard is refused"),
    ),
    request_body(
        description = "`tenant_id`, `status` (`active` or `archived`), and `name`, `min_select` and `max_select` (how many of the group a guest must and may pick), \
                       `member_item_ids` (the items it offers) and `attached_item_ids` (the items it \
                       is offered on), as they should \
                       now stand",
        content_type = "application/json",
    ),
    responses(
        (status = 200, description = "Updated. The body is the record and the ETag carries its new \
                                      version. Audited"),
        (status = 400, description = "An id is not a ULID, status is not `active` or `archived`, or \
                                      If-Match is absent, malformed, or a wildcard", body = pos_cloud_api::error_schema::ErrorResponse),
        (status = 401, description = "No session", body = pos_cloud_api::error_schema::ErrorResponse),
        (status = 403, description = "The role lacks console.catalog.manage", body = pos_cloud_api::error_schema::ErrorResponse),
        (status = 404, description = "The tenant has no modifier group with that id", body = pos_cloud_api::error_schema::ErrorResponse),
        (status = 412, description = "The group changed since you read it: re-read it and try again", body = pos_cloud_api::error_schema::ErrorResponse),
        (status = 503, description = "The catalog store is unreachable", body = pos_cloud_api::error_schema::ErrorResponse),
    ),
    tag = "modifiers",
)]
/// A super-admin renames a modifier group, sets its rule, members, attachments and/or status.
pub(super) async fn admin_update_modifier_group<Cat, A, C>(
    State(state): State<CatalogState<Cat, A, C>>,
    headers: HeaderMap,
    Path(modifier_group_id): Path<String>,
    Json(request): Json<UpdateModifierGroupRequest>,
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
    let (modifier_group_id, tenant_id) = match parse_ulid_fields([
        ("modifier_group_id", &modifier_group_id),
        ("tenant_id", &request.tenant_id),
    ]) {
        Ok([modifier_group_id, tenant_id]) => (
            ModifierGroupId::new(modifier_group_id),
            TenantId::new(tenant_id),
        ),
        Err(refusal) => return refusal,
    };
    let Ok(member_item_ids) = parse_item_id_list(&request.member_item_ids) else {
        return ulid_refusal(&["member_item_ids"]);
    };
    let Ok(attached_item_ids) = parse_item_id_list(&request.attached_item_ids) else {
        return ulid_refusal(&["attached_item_ids"]);
    };
    let Some(status) = parse_entity_status(&request.status) else {
        return entity_status_refusal();
    };
    let record = ModifierGroup {
        modifier_group_id,
        tenant_id,
        name: request.name,
        min_select: request.min_select,
        max_select: request.max_select,
        member_item_ids,
        attached_item_ids,
        status,
    };
    let expected = match if_match(&headers) {
        Ok(expected) => expected,
        Err(refusal) => return refusal,
    };
    match state
        .catalog
        .update_modifier_group(&record, &expected)
        .await
    {
        Ok(UpdateOutcome::Updated(version)) => {
            audit_action(
                &state.audit,
                &state.clock,
                &context,
                Some(record.tenant_id),
                "modifier_group.update",
                "modifier_group",
                &record.modifier_group_id.to_string(),
                None,
                serde_json::to_value(&record).ok(),
            )
            .await;
            versioned_ok(record, &version)
        }
        Ok(UpdateOutcome::VersionMismatch) => version_mismatch(),
        Ok(UpdateOutcome::NotFound) => not_found("modifier group"),
        Err(error) => catalog_error_response(&error),
    }
}
