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
use crate::auth::admin::{AdminContext, AdminStore};
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

#[utoipa::path(
    get,
    path = "/admin/people/permissions",
    responses(
        (status = 200, description = "Every permission a role may grant, as an array: each with \
                                      `id`, `group`, `risk`, `pin_required` (whether it may be \
                                      granted with approval) and `description`. The same for every \
                                      tenant"),
        (status = 401, description = "No session", body = pos_cloud_api::error_schema::ErrorResponse),
        (status = 403, description = "The role lacks console.data.read", body = pos_cloud_api::error_schema::ErrorResponse),
    ),
    tag = "people",
)]
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

#[utoipa::path(
    get,
    path = "/admin/roles",
    params(
        ("tenant_id" = String, Query, description = "The tenant whose roles to list (a 26-character ULID)"),
    ),
    responses(
        (status = 200, description = "Every role template of the tenant, as an array. A role names \
                                      permissions, never a person"),
        (status = 400, description = "tenant_id is absent or not a ULID", body = pos_cloud_api::error_schema::ErrorResponse),
        (status = 401, description = "No session", body = pos_cloud_api::error_schema::ErrorResponse),
        (status = 403, description = "The role lacks console.data.read", body = pos_cloud_api::error_schema::ErrorResponse),
        (status = 503, description = "The people store is unreachable", body = pos_cloud_api::error_schema::ErrorResponse),
    ),
    tag = "people",
)]
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

#[utoipa::path(
    get,
    path = "/admin/roles/{role_id}",
    params(
        ("role_id" = String, Path, description = "The role template's 26-character ULID"),
        ("tenant_id" = String, Query, description = "The tenant that owns it (a 26-character ULID)"),
    ),
    responses(
        (status = 200, description = "The role template, with an ETag carrying the version it was \
                                      read at — the token a PATCH must send back as If-Match"),
        (status = 400, description = "tenant_id or role_id is absent or not a ULID", body = pos_cloud_api::error_schema::ErrorResponse),
        (status = 401, description = "No session", body = pos_cloud_api::error_schema::ErrorResponse),
        (status = 403, description = "The role lacks console.data.read", body = pos_cloud_api::error_schema::ErrorResponse),
        (status = 404, description = "The tenant has no role with that id", body = pos_cloud_api::error_schema::ErrorResponse),
        (status = 503, description = "The people store is unreachable", body = pos_cloud_api::error_schema::ErrorResponse),
    ),
    tag = "people",
)]
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

#[utoipa::path(
    post,
    path = "/admin/roles",
    request_body(
        description = "`tenant_id`, `name`, `permissions` (granted directly), \
                       `permissions_with_approval` (granted only with a direct holder's approval, \
                       each PIN-flagged; absent is none) and `discount_ceiling_minor` (how much the \
                       role may discount without a manager, in the currency's minor unit; absent is \
                       no ceiling, which a store reads as zero). Every id must be in \
                       `GET /admin/people/permissions`, and none may be in both lists",
        content_type = "application/json",
    ),
    responses(
        (status = 201, description = "Created. The body is `{ \"id\": … }` and the ETag carries \
                                      the new role's version"),
        (status = 400, description = "tenant_id is absent or not a ULID; name is blank; a \
                                      permission id is unknown, not PIN-flagged where granted with \
                                      approval, or in both lists; or the ceiling is negative", body = pos_cloud_api::error_schema::ErrorResponse),
        (status = 401, description = "No session", body = pos_cloud_api::error_schema::ErrorResponse),
        (status = 403, description = "The role lacks console.people.manage", body = pos_cloud_api::error_schema::ErrorResponse),
        (status = 503, description = "The people store or the entropy source is unavailable", body = pos_cloud_api::error_schema::ErrorResponse),
    ),
    tag = "people",
)]
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
    let (tenant_id, permissions_with_approval) = match read_new_role(&request) {
        Ok(read) => read,
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
            record_role_create(&state, &context, &new_role).await;
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

/// The tenant a new role is for and what it grants with approval, refused in the order the
/// request is read: the tenant, the name, the permissions, the ceiling, then what is granted with
/// approval.
#[expect(
    clippy::result_large_err,
    reason = "the Err is an axum Response by design — it is the 400 the caller returns"
)]
fn read_new_role(request: &CreateRoleRequest) -> Result<(TenantId, Vec<String>), Response> {
    let [tenant_id] = parse_ulid_fields([("tenant_id", &request.tenant_id)])?;
    if request.name.trim().is_empty() {
        return Err(api_error_with_details(
            ErrorStatus::InvalidArgument,
            "name is required",
            &[("name", "REQUIRED")],
        ));
    }
    if let Some(unknown) = first_unknown_permission(&request.permissions) {
        return Err(api_error_with_details(
            ErrorStatus::InvalidArgument,
            format!("unknown permission id: {unknown}"),
            &[("permissions", "INVALID_ENUM_VALUE")],
        ));
    }
    if request
        .discount_ceiling_minor
        .is_some_and(|minor| minor < 0)
    {
        return Err(api_error_with_details(
            ErrorStatus::InvalidArgument,
            "discount_ceiling_minor cannot be negative",
            &[("discount_ceiling_minor", "OUT_OF_RANGE")],
        ));
    }
    let permissions_with_approval = granted_with_approval(
        &request.permissions,
        Some(&request.permissions_with_approval),
        &[],
    )?;
    Ok((TenantId::new(tenant_id), permissions_with_approval))
}

/// Records `role.create` with the id, the name, both permission sets and the ceiling (a role name
/// is not PII).
async fn record_role_create<P, R, Cfg, A, C>(
    state: &PeopleState<P, R, Cfg, A, C>,
    context: &AdminContext,
    role: &NewRoleTemplate,
) where
    C: ClockSource,
{
    let after = serde_json::json!({
        "id": role.role_template_id.to_string(),
        "name": role.name,
        "permissions": role.permissions,
        "permissions_with_approval": role.permissions_with_approval,
        "discount_ceiling_minor": role.discount_ceiling_minor,
    });
    audit_action(
        &state.audit,
        &state.clock,
        context,
        Some(role.tenant_id),
        "role.create",
        "role",
        &role.role_template_id.to_string(),
        None,
        Some(after),
    )
    .await;
}

#[utoipa::path(
    patch,
    path = "/admin/roles/{role_id}",
    params(
        ("role_id" = String, Path, description = "The role template's 26-character ULID"),
        ("If-Match" = String, Header, description = "The ETag the role was read at (ADR-0094). \
                                                     Required; a wildcard is refused"),
    ),
    request_body(
        description = "The role as it should now stand: `tenant_id`, `name`, `permissions`, \
                       `discount_ceiling_minor` and `status` (`active` or `archived`), and \
                       optionally `permissions_with_approval` — absent, the role keeps what it \
                       grants with approval, less anything `permissions` now grants directly",
        content_type = "application/json",
    ),
    responses(
        (status = 200, description = "Archived. An archived role grants nothing, so every store where \
                                      someone holds it is published to at once (ADR-0158 decision \
                                      7): `stores` lists each store with its `outcome`, \
                                      `PERMISSIONS_PUBLISH_APPLIED` with its `config_version_id` or \
                                      `PERMISSIONS_PUBLISH_FAILED`, which `POST /admin/people/publish` \
                                      retries. The ETag carries the new version"),
        (status = 204, description = "Updated: a rename, a changed permission set or ceiling, or \
                                      restoring the role. Nothing is published until someone \
                                      publishes. The ETag carries the new version"),
        (status = 400, description = "tenant_id or role_id is absent or not a ULID; name is blank; a \
                                      permission id is unknown, not PIN-flagged where granted with \
                                      approval, or in both lists; status is not `active` or \
                                      `archived`; the ceiling is negative; or If-Match is absent, \
                                      malformed, or a wildcard", body = pos_cloud_api::error_schema::ErrorResponse),
        (status = 401, description = "No session", body = pos_cloud_api::error_schema::ErrorResponse),
        (status = 403, description = "The role lacks console.people.manage", body = pos_cloud_api::error_schema::ErrorResponse),
        (status = 404, description = "The tenant has no role with that id", body = pos_cloud_api::error_schema::ErrorResponse),
        (status = 412, description = "The role changed since you read it: re-read it and try again", body = pos_cloud_api::error_schema::ErrorResponse),
        (status = 503, description = "The people store, the registry or the store groups is \
                                      unreachable", body = pos_cloud_api::error_schema::ErrorResponse),
    ),
    tag = "people",
)]
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
