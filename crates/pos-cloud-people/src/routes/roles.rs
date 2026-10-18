// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! `/admin/permissions` and `/admin/roles`: the permission catalogue, and a tenant's role templates.

use axum::Json;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use pos_cloud_api::console::{audit_action, mint_ulid, require_permission};
use pos_cloud_api::response::{api_error_with_details, parse_ulid_fields};
use pos_proto::ErrorStatus;
use pos_proto::determinism::ClockSource;
use pos_proto::ids::TenantId;

use super::PeopleState;
use super::checks::planned_role_update;
use super::requests::{
    CreateRoleRequest, PeopleTenantQuery, UpdateRoleRequest, people_entropy_unavailable,
    people_error_response,
};
use crate::auth::admin::AdminStore;
use crate::auth::console_rbac::ConsolePermission;
use crate::config_tree::ConfigTreeStore;
use crate::people::{
    AssignmentStore, EmployeeStore, NewRoleTemplate, PermissionInfo, RoleTemplateId,
    RoleTemplateStore, is_known_permission, is_pin_flagged_permission, permission_catalogue,
};
use crate::registry::RegistryStore;
use crate::version::UpdateOutcome;
use pos_cloud_api::conditional::{if_match, version_mismatch, versioned_ok, with_etag};
use pos_cloud_api::refusal::not_found;
use pos_cloud_config::route::{entity_status_refusal, parse_entity_status};

/// The `pos-core` permission catalogue (§9), for the console's role editor to offer. Behind `Read` and
/// tenant-independent — the catalogue is the same for every tenant.
pub(super) async fn admin_list_permissions<P, R, Cfg, A, C>(
    State(state): State<PeopleState<P, R, Cfg, A, C>>,
    headers: HeaderMap,
) -> Response
where
    P: Clone + Send + Sync + 'static,
    R: Clone + Send + Sync + 'static,
    Cfg: Clone + Send + Sync + 'static,
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
    (
        StatusCode::OK,
        Json::<Vec<PermissionInfo>>(permission_catalogue()),
    )
        .into_response()
}

/// A super-admin lists a tenant's role templates.
pub(super) async fn admin_list_roles<P, R, Cfg, A, C>(
    State(state): State<PeopleState<P, R, Cfg, A, C>>,
    headers: HeaderMap,
    Query(query): Query<PeopleTenantQuery>,
) -> Response
where
    P: RoleTemplateStore + Clone + Send + Sync + 'static,
    R: Clone + Send + Sync + 'static,
    Cfg: Clone + Send + Sync + 'static,
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
    match state.people.list(tenant_id).await {
        Ok(roles) => (StatusCode::OK, Json(roles)).into_response(),
        Err(error) => people_error_response(&error),
    }
}

/// A super-admin reads one role template within its tenant.
pub(super) async fn admin_get_role<P, R, Cfg, A, C>(
    State(state): State<PeopleState<P, R, Cfg, A, C>>,
    headers: HeaderMap,
    Path(role_id): Path<String>,
    Query(query): Query<PeopleTenantQuery>,
) -> Response
where
    P: RoleTemplateStore + Clone + Send + Sync + 'static,
    R: Clone + Send + Sync + 'static,
    Cfg: Clone + Send + Sync + 'static,
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
    let (tenant_id, role_id) =
        match parse_ulid_fields([("tenant_id", &query.tenant_id), ("role_id", &role_id)]) {
            Ok([tenant_id, role_id]) => (TenantId::new(tenant_id), RoleTemplateId::new(role_id)),
            Err(refusal) => return refusal,
        };
    match state.people.get(tenant_id, role_id).await {
        Ok(Some(role)) => versioned_ok(role.record, &role.etag),
        Ok(None) => not_found("role"),
        Err(error) => people_error_response(&error),
    }
}

/// Validates that every permission id is in the `pos-core` catalogue (§9); returns the first unknown.
fn first_unknown_permission(permissions: &[String]) -> Option<&str> {
    permissions
        .iter()
        .map(String::as_str)
        .find(|id| !is_known_permission(id))
}

/// What a role write stores as granted with approval, or why it is refused
/// ([ADR-0158](../../../docs/adr/0158-the-till-enforces-each-persons-own-permissions.md)
/// decision 4). `permissions` is what the role grants directly, stored as sent; it has already
/// passed [`first_unknown_permission`].
///
/// `listed` is the request's own list. Each id in it must be in the catalogue and PIN-flagged, and
/// none may also be granted directly: a permission is held one way. A create always has one, an
/// absent list reading as none. An update may leave it out, as one written before the list existed
/// does, and the role then keeps what it grants with approval today (`stored`), less anything
/// `permissions` now grants directly.
#[expect(
    clippy::result_large_err,
    reason = "the Err is an axum Response by design — it *is* the 400 the caller returns"
)]
pub(super) fn granted_with_approval(
    permissions: &[String],
    listed: Option<&[String]>,
    stored: &[String],
) -> Result<Vec<String>, Response> {
    let Some(listed) = listed else {
        let kept = stored.iter().filter(|id| !permissions.contains(id));
        return Ok(kept.cloned().collect());
    };
    if let Some(unknown) = first_unknown_permission(listed) {
        return Err(api_error_with_details(
            ErrorStatus::InvalidArgument,
            format!("unknown permission id: {unknown}"),
            &[("permissions_with_approval", "INVALID_ENUM_VALUE")],
        ));
    }
    if let Some(unflagged) = listed.iter().find(|id| !is_pin_flagged_permission(id)) {
        return Err(api_error_with_details(
            ErrorStatus::InvalidArgument,
            format!("only a PIN-flagged permission is granted with approval: {unflagged}"),
            &[("permissions_with_approval", "INVALID_VALUE")],
        ));
    }
    if let Some(both) = listed.iter().find(|id| permissions.contains(id)) {
        return Err(api_error_with_details(
            ErrorStatus::InvalidArgument,
            format!("a permission is granted directly or with approval, not both: {both}"),
            &[
                ("permissions", "MUTUALLY_EXCLUSIVE"),
                ("permissions_with_approval", "MUTUALLY_EXCLUSIVE"),
            ],
        ));
    }
    Ok(listed.to_vec())
}

/// A super-admin creates a role template. Both permission sets are validated against the catalogue
/// ([`granted_with_approval`]). Audited `role.create` with id, name, both sets and the ceiling (a
/// role name is not PII).
pub(super) async fn admin_create_role<P, R, Cfg, A, C>(
    State(state): State<PeopleState<P, R, Cfg, A, C>>,
    headers: HeaderMap,
    Json(request): Json<CreateRoleRequest>,
) -> Response
where
    P: RoleTemplateStore + Clone + Send + Sync + 'static,
    R: Clone + Send + Sync + 'static,
    Cfg: Clone + Send + Sync + 'static,
    A: AdminStore + Clone + Send + Sync + 'static,
    C: ClockSource + Clone + Send + Sync + 'static,
{
    let context = match require_permission(
        &state.admin,
        &state.clock,
        &headers,
        ConsolePermission::ManagePeople,
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
    if request.name.trim().is_empty() {
        return api_error_with_details(
            ErrorStatus::InvalidArgument,
            "name is required",
            &[("name", "REQUIRED")],
        );
    }
    if let Some(unknown) = first_unknown_permission(&request.permissions) {
        return api_error_with_details(
            ErrorStatus::InvalidArgument,
            format!("unknown permission id: {unknown}"),
            &[("permissions", "INVALID_ENUM_VALUE")],
        );
    }
    if request
        .discount_ceiling_minor
        .is_some_and(|minor| minor < 0)
    {
        return api_error_with_details(
            ErrorStatus::InvalidArgument,
            "discount_ceiling_minor cannot be negative",
            &[("discount_ceiling_minor", "OUT_OF_RANGE")],
        );
    }
    let permissions_with_approval = match granted_with_approval(
        &request.permissions,
        Some(&request.permissions_with_approval),
        &[],
    ) {
        Ok(listed) => listed,
        Err(refusal) => return refusal,
    };
    let Some(role_template_id) =
        mint_ulid(state.clock.now().as_milliseconds_since_epoch()).map(RoleTemplateId::new)
    else {
        return people_entropy_unavailable();
    };
    let new_role = NewRoleTemplate {
        role_template_id,
        tenant_id,
        name: request.name.clone(),
        permissions: request.permissions.clone(),
        permissions_with_approval,
        discount_ceiling_minor: request.discount_ceiling_minor,
    };
    match state.people.create(&new_role).await {
        Ok(version) => {
            let after = serde_json::json!({
                "id": role_template_id.to_string(),
                "name": new_role.name,
                "permissions": new_role.permissions,
                "permissions_with_approval": new_role.permissions_with_approval,
                "discount_ceiling_minor": new_role.discount_ceiling_minor,
            });
            audit_action(
                &state.audit,
                &state.clock,
                &context,
                Some(tenant_id),
                "role.create",
                "role",
                &role_template_id.to_string(),
                None,
                Some(after),
            )
            .await;
            with_etag(
                (
                    StatusCode::CREATED,
                    Json(serde_json::json!({ "id": role_template_id.to_string() })),
                )
                    .into_response(),
                &version,
            )
        }
        Err(error) => people_error_response(&error),
    }
}

/// A super-admin updates a role template's name, permission sets, ceiling and status. Audited
/// `role.update` with what was written, both sets included ([`granted_with_approval`]).
///
/// An archived role grants nothing, so archiving one publishes the `permissions` node at once to
/// every store where someone holds it
/// ([ADR-0158](../../../docs/adr/0158-the-till-enforces-each-persons-own-permissions.md)
/// decision 7), and the answer is then `200` with each store's outcome. Any other update — a
/// rename, a changed permission set or ceiling, or restoring the role — publishes as before, and
/// answers `204`.
pub(super) async fn admin_update_role<P, R, Cfg, A, C>(
    State(state): State<PeopleState<P, R, Cfg, A, C>>,
    headers: HeaderMap,
    Path(role_id): Path<String>,
    Json(request): Json<UpdateRoleRequest>,
) -> Response
where
    P: EmployeeStore + RoleTemplateStore + AssignmentStore + Clone + Send + Sync + 'static,
    R: RegistryStore + Clone + Send + Sync + 'static,
    Cfg: ConfigTreeStore + Clone + Send + Sync + 'static,
    A: AdminStore + Clone + Send + Sync + 'static,
    C: ClockSource + Clone + Send + Sync + 'static,
{
    let context = match require_permission(
        &state.admin,
        &state.clock,
        &headers,
        ConsolePermission::ManagePeople,
    )
    .await
    {
        Ok(context) => context,
        Err(denied) => return denied,
    };
    let (tenant_id, role_template_id) =
        match parse_ulid_fields([("tenant_id", &request.tenant_id), ("role_id", &role_id)]) {
            Ok([tenant_id, role_template_id]) => (
                TenantId::new(tenant_id),
                RoleTemplateId::new(role_template_id),
            ),
            Err(refusal) => return refusal,
        };
    if request.name.trim().is_empty() {
        return api_error_with_details(
            ErrorStatus::InvalidArgument,
            "name is required",
            &[("name", "REQUIRED")],
        );
    }
    if let Some(unknown) = first_unknown_permission(&request.permissions) {
        return api_error_with_details(
            ErrorStatus::InvalidArgument,
            format!("unknown permission id: {unknown}"),
            &[("permissions", "INVALID_ENUM_VALUE")],
        );
    }
    let Some(status) = parse_entity_status(&request.status) else {
        return entity_status_refusal();
    };
    if request
        .discount_ceiling_minor
        .is_some_and(|minor| minor < 0)
    {
        return api_error_with_details(
            ErrorStatus::InvalidArgument,
            "discount_ceiling_minor cannot be negative",
            &[("discount_ceiling_minor", "OUT_OF_RANGE")],
        );
    }
    let planned = planned_role_update(&state, tenant_id, role_template_id, &request, status).await;
    let (update, reached) = match planned {
        Ok(planned) => planned,
        Err(refusal) => return refusal,
    };
    let expected = match if_match(&headers) {
        Ok(expected) => expected,
        Err(refusal) => return refusal,
    };
    match RoleTemplateStore::update(&state.people, &update, &expected).await {
        Ok(UpdateOutcome::Updated(version)) => {
            let after = serde_json::json!({
                "id": role_template_id.to_string(),
                "name": update.name,
                "permissions": update.permissions,
                "permissions_with_approval": update.permissions_with_approval,
                "discount_ceiling_minor": update.discount_ceiling_minor,
                "status": status.as_str(),
            });
            audit_action(
                &state.audit,
                &state.clock,
                &context,
                Some(tenant_id),
                "role.update",
                "role",
                &role_template_id.to_string(),
                None,
                Some(after),
            )
            .await;
            let cause = ("role_template_id", role_template_id.to_string());
            state
                .answer_update(&context, tenant_id, &version, reached, cause)
                .await
        }
        Ok(UpdateOutcome::VersionMismatch) => version_mismatch(),
        Ok(UpdateOutcome::NotFound) => not_found("role"),
        Err(error) => people_error_response(&error),
    }
}
