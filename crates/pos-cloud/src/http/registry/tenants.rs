// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! `/admin/tenants`: list, create and update tenants, and the roles a new tenant starts with.

use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use pos_cloud_api::console::{audit_action, mint_ulid, require_permission};
use pos_cloud_api::response::parse_ulid_fields;
use pos_proto::determinism::ClockSource;
use pos_proto::ids::TenantId;

use super::{
    CreateTenantRequest, RegistryState, UpdateEntityRequest, registry_entropy_unavailable,
    registry_error_response,
};
use crate::auth::admin::{AdminContext, AdminStore};
use crate::auth::console_rbac::ConsolePermission;
use crate::http::{
    Json, entity_status_refusal, if_match, not_found, parse_entity_status, version_mismatch,
    versioned_created, versioned_ok,
};
use crate::registry::{EntityStatus, RegistryStore, TenantRecord};
use crate::version::UpdateOutcome;

#[utoipa::path(
    get,
    path = "/admin/tenants",
    security(("session_cookie" = [])),
    responses(
        (status = 200, description = "Every tenant, as an array: `tenant_id`, `name` and `status`, \
                                      each with the `etag` its version is at"),
        (status = 401, description = "No session", body = crate::openapi_admin::ErrorResponse),
        (status = 403, description = "The role lacks console.read", body = crate::openapi_admin::ErrorResponse),
        (status = 503, description = "The registry store failed", body = crate::openapi_admin::ErrorResponse),
    ),
    tag = "registry",
)]
/// A super-admin lists every tenant.
pub(super) async fn admin_list_tenants<Rg, A, C>(
    State(state): State<RegistryState<Rg, A, C>>,
    headers: HeaderMap,
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
    match state.registry.list_tenants().await {
        Ok(tenants) => (StatusCode::OK, Json(tenants)).into_response(),
        Err(error) => registry_error_response(&error),
    }
}

#[utoipa::path(
    post,
    path = "/admin/tenants",
    security(("session_cookie" = [])),
    request_body(
        description = "The tenant's `name`, and optionally `language`, a language tag such as `vi` \
                       that the tenant's starting roles are named in. Absent, or one the console's \
                       language packs do not carry, names them in English",
        content_type = "application/json",
    ),
    responses(
        (status = 201, description = "Created, active, with the id minted here. The body is the \
                                      record and the ETag carries its version. The tenant is given \
                                      its six starting roles (ADR-0158); if that fails the tenant \
                                      still stands, with fewer roles"),
        (status = 401, description = "No session", body = crate::openapi_admin::ErrorResponse),
        (status = 403, description = "The role lacks console.orgs.manage", body = crate::openapi_admin::ErrorResponse),
        (status = 503, description = "The registry store or the entropy source is unavailable", body = crate::openapi_admin::ErrorResponse),
    ),
    tag = "registry",
)]
/// A super-admin creates a tenant; the id is minted here and returned once in the created record.
///
/// The tenant then gets its six starting roles, one for each of `pos-core`'s built-in roles,
/// granting what the catalogue's defaults say ([`crate::starting_roles`],
/// [ADR-0158](../../../docs/adr/0158-the-till-enforces-each-persons-own-permissions.md)). They are
/// audited as one `role.seed` entry carrying their ids and counts. The tenant exists whatever the
/// seeding does, so a seeding failure is logged rather than refused: the tenant then starts with
/// fewer roles, and its owner creates the rest in People.
pub(super) async fn admin_create_tenant<Rg, A, C>(
    State(state): State<RegistryState<Rg, A, C>>,
    headers: HeaderMap,
    Json(request): Json<CreateTenantRequest>,
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
    let Some(tenant_id) =
        mint_ulid(state.clock.now().as_milliseconds_since_epoch()).map(TenantId::new)
    else {
        return registry_entropy_unavailable();
    };
    let record = TenantRecord {
        tenant_id,
        name: request.name,
        status: EntityStatus::Active,
    };
    match state.registry.create_tenant(&record).await {
        Ok(version) => {
            let after = serde_json::to_value(&record).ok();
            audit_action(
                &state.audit,
                &state.clock,
                &context,
                Some(tenant_id),
                "tenant.create",
                "tenant",
                &tenant_id.to_string(),
                None,
                after,
            )
            .await;
            seed_tenant_roles(&state, &context, tenant_id, request.language.as_deref()).await;
            versioned_created(record, &version)
        }
        Err(error) => registry_error_response(&error),
    }
}

/// Gives a tenant just created its starting roles, and audits what was written as `role.seed`: the
/// ids and the counts, nothing else. A failure is logged with the tenant's id; the tenant stands.
async fn seed_tenant_roles<Rg, A, C>(
    state: &RegistryState<Rg, A, C>,
    context: &AdminContext,
    tenant_id: TenantId,
    language: Option<&str>,
) where
    C: ClockSource,
{
    match state.starting_roles.seed(tenant_id, language).await {
        Ok(seeded) if seeded.created.is_empty() => {}
        Ok(seeded) => {
            let after = serde_json::json!({
                "role_template_ids": seeded
                    .created
                    .iter()
                    .map(ToString::to_string)
                    .collect::<Vec<_>>(),
                "created": seeded.created.len(),
                "kept": seeded.kept.len(),
            });
            audit_action(
                &state.audit,
                &state.clock,
                context,
                Some(tenant_id),
                "role.seed",
                "tenant",
                &tenant_id.to_string(),
                None,
                Some(after),
            )
            .await;
        }
        Err(error) => {
            tracing::error!(%error, %tenant_id, "a new tenant's starting roles could not be written");
        }
    }
}

#[utoipa::path(
    patch,
    path = "/admin/tenants/{tenant_id}",
    security(("session_cookie" = [])),
    params(
        ("tenant_id" = String, Path, description = "The tenant (a 26-character ULID)"),
        ("If-Match" = String, Header, description = "The ETag the record was read at (ADR-0094). \
                                                     Required; a wildcard is refused"),
    ),
    request_body(
        description = "The `name` and `status` (`active` or `archived`) as they should now stand",
        content_type = "application/json",
    ),
    responses(
        (status = 200, description = "Updated. The body is the record and the ETag carries its new \
                                      version"),
        (status = 400, description = "tenant_id is not a ULID, status is not `active` or \
                                      `archived`, or If-Match is absent, malformed, or a wildcard", body = crate::openapi_admin::ErrorResponse),
        (status = 401, description = "No session", body = crate::openapi_admin::ErrorResponse),
        (status = 403, description = "The role lacks console.orgs.manage", body = crate::openapi_admin::ErrorResponse),
        (status = 404, description = "There is no tenant with that id", body = crate::openapi_admin::ErrorResponse),
        (status = 412, description = "The tenant changed since you read it: re-read it and try again", body = crate::openapi_admin::ErrorResponse),
        (status = 503, description = "The registry store failed", body = crate::openapi_admin::ErrorResponse),
    ),
    tag = "registry",
)]
/// A super-admin renames a tenant and/or sets its status.
pub(super) async fn admin_update_tenant<Rg, A, C>(
    State(state): State<RegistryState<Rg, A, C>>,
    headers: HeaderMap,
    Path(tenant_id): Path<String>,
    Json(request): Json<UpdateEntityRequest>,
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
    let tenant_id = match parse_ulid_fields([("tenant_id", &tenant_id)]) {
        Ok([tenant_id]) => TenantId::new(tenant_id),
        Err(refusal) => return refusal,
    };
    let Some(status) = parse_entity_status(&request.status) else {
        return entity_status_refusal();
    };
    let record = TenantRecord {
        tenant_id,
        name: request.name,
        status,
    };
    let expected = match if_match(&headers) {
        Ok(expected) => expected,
        Err(refusal) => return refusal,
    };
    match state.registry.update_tenant(&record, &expected).await {
        Ok(UpdateOutcome::Updated(version)) => {
            let after = serde_json::to_value(&record).ok();
            audit_action(
                &state.audit,
                &state.clock,
                &context,
                Some(tenant_id),
                "tenant.update",
                "tenant",
                &tenant_id.to_string(),
                None,
                after,
            )
            .await;
            versioned_ok(record, &version)
        }
        Ok(UpdateOutcome::VersionMismatch) => version_mismatch(),
        Ok(UpdateOutcome::NotFound) => not_found("tenant"),
        Err(error) => registry_error_response(&error),
    }
}
