// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! `/admin/brands`: list, create and update a tenant's brands.

use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use pos_cloud_api::console::{audit_action, mint_ulid, require_permission};
use pos_cloud_api::response::parse_ulid_fields;
use pos_proto::determinism::ClockSource;
use pos_proto::ids::TenantId;

use super::{
    CreateBrandRequest, RegistryState, UpdateBrandRequest, registry_entropy_unavailable,
    registry_error_response,
};
use crate::auth::admin::AdminStore;
use crate::auth::console_rbac::ConsolePermission;
use crate::http::{
    Json, RegistryTenantQuery, entity_status_refusal, if_match, not_found, parse_entity_status,
    version_mismatch, versioned_created, versioned_ok,
};
use crate::registry::{BrandId, BrandRecord, EntityStatus, RegistryStore};
use crate::version::UpdateOutcome;
use crate::version::Versioned;

#[utoipa::path(
    get,
    path = "/admin/brands",
    extensions(("x-permission" = json!("console.data.read"))),
    security(("session_cookie" = [])),
    params(
        ("tenant_id" = String, Query, description = "The tenant whose brands to list (a 26-character ULID)"),
    ),
    responses(
        (status = 200, body = [Versioned<BrandRecord>], description = "The tenant's brands, as an array: `brand_id`, `tenant_id`, \
                                      `name` and `status`, each with its `etag`"),
        (status = 400, description = "tenant_id is absent or not a ULID", body = crate::openapi_admin::ErrorResponse),
        (status = 401, description = "No session", body = crate::openapi_admin::ErrorResponse),
        (status = 403, description = "The role lacks console.data.read", body = crate::openapi_admin::ErrorResponse),
        (status = 503, description = "The registry store failed", body = crate::openapi_admin::ErrorResponse),
    ),
    tag = "registry",
)]
/// A super-admin lists a tenant's brands.
pub(super) async fn admin_list_brands<Rg, A, C>(
    State(state): State<RegistryState<Rg, A, C>>,
    headers: HeaderMap,
    Query(query): Query<RegistryTenantQuery>,
) -> Response
where
    Rg: RegistryStore + Clone + Send + Sync + 'static,
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
    match state.registry.list_brands(tenant_id).await {
        Ok(brands) => (StatusCode::OK, Json(brands)).into_response(),
        Err(error) => registry_error_response(&error),
    }
}

#[utoipa::path(
    post,
    path = "/admin/brands",
    extensions(("x-permission" = json!("console.orgs.manage"))),
    security(("session_cookie" = [])),
    request_body(
        content = CreateBrandRequest, description = "The `tenant_id` the brand belongs to and its `name`",
        content_type = "application/json",
    ),
    responses(
        (status = 201, body = Versioned<BrandRecord>, description = "Created, active, with the id minted here. The body is the \
                                      record and the ETag carries its version"),
        (status = 400, description = "tenant_id is not a ULID", body = crate::openapi_admin::ErrorResponse),
        (status = 401, description = "No session", body = crate::openapi_admin::ErrorResponse),
        (status = 403, description = "The role lacks console.orgs.manage", body = crate::openapi_admin::ErrorResponse),
        (status = 503, description = "The registry store or the entropy source is unavailable", body = crate::openapi_admin::ErrorResponse),
    ),
    tag = "registry",
)]
/// A super-admin creates a brand under a tenant.
pub(super) async fn admin_create_brand<Rg, A, C>(
    State(state): State<RegistryState<Rg, A, C>>,
    headers: HeaderMap,
    Json(request): Json<CreateBrandRequest>,
) -> Response
where
    Rg: RegistryStore + Clone + Send + Sync + 'static,
    A: AdminStore + Clone + Send + Sync + 'static,
    C: ClockSource + Clone + Send + Sync + 'static,
{
    let context = match require_permission(
        &state.admin,
        &state.clock,
        &headers,
        ConsolePermission::ManageOrgs,
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
    let Some(brand_id) =
        mint_ulid(state.clock.now().as_milliseconds_since_epoch()).map(BrandId::new)
    else {
        return registry_entropy_unavailable();
    };
    let record = BrandRecord {
        brand_id,
        tenant_id,
        name: request.name,
        status: EntityStatus::Active,
    };
    match state.registry.create_brand(&record).await {
        Ok(version) => {
            let after = serde_json::to_value(&record).ok();
            audit_action(
                &state.audit,
                &state.clock,
                &context,
                Some(tenant_id),
                "brand.create",
                "brand",
                &brand_id.to_string(),
                None,
                after,
            )
            .await;
            versioned_created(record, &version)
        }
        Err(error) => registry_error_response(&error),
    }
}

#[utoipa::path(
    patch,
    path = "/admin/brands/{brand_id}",
    extensions(("x-permission" = json!("console.orgs.manage"))),
    security(("session_cookie" = [])),
    params(
        ("brand_id" = String, Path, description = "The brand (a 26-character ULID)"),
        ("If-Match" = String, Header, description = "The ETag the record was read at (ADR-0094). \
                                                     Required; a wildcard is refused"),
    ),
    request_body(
        content = UpdateBrandRequest, description = "The `tenant_id` that owns the brand, and its `name` and `status` (`active` or \
                       `archived`) as they should now stand",
        content_type = "application/json",
    ),
    responses(
        (status = 200, body = Versioned<BrandRecord>, description = "Updated. The body is the record and the ETag carries its new \
                                      version"),
        (status = 400, description = "brand_id or tenant_id is not a ULID, status is not `active` or \
                                      `archived`, or If-Match is absent, malformed, or a wildcard", body = crate::openapi_admin::ErrorResponse),
        (status = 401, description = "No session", body = crate::openapi_admin::ErrorResponse),
        (status = 403, description = "The role lacks console.orgs.manage", body = crate::openapi_admin::ErrorResponse),
        (status = 404, description = "The tenant has no brand with that id", body = crate::openapi_admin::ErrorResponse),
        (status = 412, description = "The brand changed since you read it: re-read it and try again", body = crate::openapi_admin::ErrorResponse),
        (status = 503, description = "The registry store failed", body = crate::openapi_admin::ErrorResponse),
    ),
    tag = "registry",
)]
/// A super-admin renames a brand and/or sets its status.
pub(super) async fn admin_update_brand<Rg, A, C>(
    State(state): State<RegistryState<Rg, A, C>>,
    headers: HeaderMap,
    Path(brand_id): Path<String>,
    Json(request): Json<UpdateBrandRequest>,
) -> Response
where
    Rg: RegistryStore + Clone + Send + Sync + 'static,
    A: AdminStore + Clone + Send + Sync + 'static,
    C: ClockSource + Clone + Send + Sync + 'static,
{
    let context = match require_permission(
        &state.admin,
        &state.clock,
        &headers,
        ConsolePermission::ManageOrgs,
    )
    .await
    {
        Ok(context) => context,
        Err(denied) => return denied,
    };
    let (brand_id, tenant_id) =
        match parse_ulid_fields([("brand_id", &brand_id), ("tenant_id", &request.tenant_id)]) {
            Ok([brand_id, tenant_id]) => (BrandId::new(brand_id), TenantId::new(tenant_id)),
            Err(refusal) => return refusal,
        };
    let Some(status) = parse_entity_status(&request.status) else {
        return entity_status_refusal();
    };
    let record = BrandRecord {
        brand_id,
        tenant_id,
        name: request.name,
        status,
    };
    let expected = match if_match(&headers) {
        Ok(expected) => expected,
        Err(refusal) => return refusal,
    };
    match state.registry.update_brand(&record, &expected).await {
        Ok(UpdateOutcome::Updated(version)) => {
            let after = serde_json::to_value(&record).ok();
            audit_action(
                &state.audit,
                &state.clock,
                &context,
                Some(tenant_id),
                "brand.update",
                "brand",
                &brand_id.to_string(),
                None,
                after,
            )
            .await;
            versioned_ok(record, &version)
        }
        Ok(UpdateOutcome::VersionMismatch) => version_mismatch(),
        Ok(UpdateOutcome::NotFound) => not_found("brand"),
        Err(error) => registry_error_response(&error),
    }
}
