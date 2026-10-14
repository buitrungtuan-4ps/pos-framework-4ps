// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The `/admin/reason-codes` handlers: list, read, create, update and delete.

use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use pos_cloud_api::console::{audit_action, mint_ulid, require_permission};
use pos_cloud_api::response::parse_ulid_fields;
use pos_proto::determinism::ClockSource;
use pos_proto::ids::{ReasonCodeId, TenantId};
use pos_proto::reason_codes::{PublishedReasonCode, ReasonAction, ReasonCode};
use pos_proto::text::DisplayName;
use pos_proto::wire_enum::Open;

use super::{
    ReasonCodeAuditSummary, ReasonCodeRequest, ReasonCodeState, reason_code_entropy_unavailable,
    reason_code_error_response,
};
use crate::auth::admin::AdminStore;
use crate::auth::console_rbac::ConsolePermission;
use crate::http::catalog::clean_name_translations;
use crate::http::{
    FieldRefusal, Json, RegistryTenantQuery, already_exists, if_match, not_found, version_mismatch,
    versioned_created, versioned_ok,
};
use crate::reason_codes::ReasonCodeStore;
use crate::version::{CreateOutcome, UpdateOutcome};

/// Validates a reason-code request and builds the entry with the given (server-owned) id. Returns
/// the fault message for a `400` rather than the whole response, so the error type stays small.
///
/// An entry valid for no action is refused here, not only at publish: it can be picked for nothing,
/// so it is a data-entry mistake wherever it is caught, and catching it in the form the operator is
/// typing into names the field while they can still fix it. The publish keeps its own check
/// (ADR-0115), because that is the guarantee the edge relies on.
fn build_reason_code(
    request: &ReasonCodeRequest,
    id: ReasonCodeId,
) -> Result<PublishedReasonCode, FieldRefusal> {
    let code = request.code.trim();
    if code.is_empty() {
        return Err(FieldRefusal::new("code is required", "code", "REQUIRED"));
    }
    let display_name = request.display_name.trim();
    if display_name.is_empty() {
        return Err(FieldRefusal::new(
            "display_name is required",
            "display_name",
            "REQUIRED",
        ));
    }
    let mut applies_to: Vec<Open<ReasonAction>> = Vec::new();
    for action in &request.applies_to {
        if action.is_unspecified() || action.is_unrecognised() {
            // `INVALID_ENUM_VALUE`, not `REQUIRED`: an empty list is answered below, so what
            // reaches here is a present token that is unrecognised or explicitly `UNSPECIFIED`.
            // An unrecognised action matches nothing at the till, so storing one would author a
            // reason that silently applies to nothing.
            return Err(FieldRefusal::new(
                "a reason names an unknown action",
                "applies_to",
                "INVALID_ENUM_VALUE",
            ));
        }
        if !applies_to.contains(action) {
            applies_to.push(action.clone());
        }
    }
    if applies_to.is_empty() {
        return Err(FieldRefusal::new(
            "a reason must be valid for at least one action",
            "applies_to",
            "REQUIRED",
        ));
    }
    Ok(PublishedReasonCode {
        id,
        code: ReasonCode::new(code),
        display_name: DisplayName::new(display_name),
        display_name_translations: clean_name_translations(
            request.display_name_translations.clone(),
        )
        .into_iter()
        .map(|(locale, name)| (locale, DisplayName::new(name)))
        .collect(),
        applies_to,
        active: request.active,
    })
}

#[utoipa::path(
    get,
    path = "/admin/reason-codes",
    params(("tenant_id" = String, Query, description = "The tenant whose list to read (a 26-character ULID)")),
    responses(
        (status = 200, description = "The tenant's entries in id order, each with the version it \
                                      was read at. Retired entries are included, because a \
                                      historic event still names them and the console has to show \
                                      what is out of service"),
        (status = 400, description = "tenant_id is absent or not a ULID", body = crate::openapi_admin::ErrorResponse),
        (status = 401, description = "No session", body = crate::openapi_admin::ErrorResponse),
        (status = 403, description = "The role lacks console.read", body = crate::openapi_admin::ErrorResponse),
        (status = 503, description = "The reason code store is unreachable", body = crate::openapi_admin::ErrorResponse),
    ),
    tag = "reason codes",
)]
/// A super-admin lists a tenant's authored reason codes, retired ones included.
pub(super) async fn admin_list_reason_codes<R, A, C>(
    State(state): State<ReasonCodeState<R, A, C>>,
    headers: HeaderMap,
    Query(query): Query<RegistryTenantQuery>,
) -> Response
where
    R: ReasonCodeStore + Clone + Send + Sync + 'static,
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
    match state.reason_codes.list(tenant_id).await {
        Ok(rows) => (StatusCode::OK, Json(rows)).into_response(),
        Err(error) => reason_code_error_response(&error),
    }
}

#[utoipa::path(
    get,
    path = "/admin/reason-codes/{reason_code_id}",
    params(
        ("reason_code_id" = String, Path, description = "The entry's 26-character ULID"),
        ("tenant_id" = String, Query, description = "The tenant that owns it (a 26-character ULID)"),
    ),
    responses(
        (status = 200, description = "The entry, with an ETag carrying the version it was read at \
                                      — the token a PUT must send back as If-Match"),
        (status = 400, description = "tenant_id or reason_code_id is absent or not a ULID", body = crate::openapi_admin::ErrorResponse),
        (status = 401, description = "No session", body = crate::openapi_admin::ErrorResponse),
        (status = 403, description = "The role lacks console.read", body = crate::openapi_admin::ErrorResponse),
        (status = 404, description = "The tenant has no entry with that id", body = crate::openapi_admin::ErrorResponse),
        (status = 503, description = "The reason code store is unreachable", body = crate::openapi_admin::ErrorResponse),
    ),
    tag = "reason codes",
)]
/// A super-admin reads one reason code by id.
pub(super) async fn admin_get_reason_code<R, A, C>(
    State(state): State<ReasonCodeState<R, A, C>>,
    headers: HeaderMap,
    Path(reason_code_id): Path<String>,
    Query(query): Query<RegistryTenantQuery>,
) -> Response
where
    R: ReasonCodeStore + Clone + Send + Sync + 'static,
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
    let (tenant_id, reason_code_id) = match parse_ulid_fields([
        ("tenant_id", &query.tenant_id),
        ("reason_code_id", &reason_code_id),
    ]) {
        Ok([tenant_id, reason_code_id]) => {
            (TenantId::new(tenant_id), ReasonCodeId::new(reason_code_id))
        }
        Err(refusal) => return refusal,
    };
    match state.reason_codes.get(tenant_id, reason_code_id).await {
        Ok(Some(row)) => versioned_ok(row.record, &row.etag),
        Ok(None) => not_found("reason code"),
        Err(error) => reason_code_error_response(&error),
    }
}

#[utoipa::path(
    post,
    path = "/admin/reason-codes",
    request_body(
        description = "The tenant, the report handle, the name staff read, its per-locale names, \
                       and the actions it may be cited for. The id is server-minted and must not \
                       be sent; `active` defaults to true",
        content_type = "application/json",
    ),
    responses(
        (status = 201, description = "Created. The body is the entry as stored, including the id \
                                      the server minted, and the ETag carries its first version"),
        (status = 400, description = "code or display_name is blank, applies_to is empty, or it \
                                      names an action this cloud does not know", body = crate::openapi_admin::ErrorResponse),
        (status = 401, description = "No session", body = crate::openapi_admin::ErrorResponse),
        (status = 403, description = "The role lacks console.reason_codes.manage", body = crate::openapi_admin::ErrorResponse),
        (status = 409, description = "An entry already holds that id", body = crate::openapi_admin::ErrorResponse),
        (status = 503, description = "The reason code store is unreachable, or the OS would not \
                                      give the entropy the id needs", body = crate::openapi_admin::ErrorResponse),
    ),
    tag = "reason codes",
)]
/// A super-admin authors a reason code; the server mints its id.
pub(super) async fn admin_create_reason_code<R, A, C>(
    State(state): State<ReasonCodeState<R, A, C>>,
    headers: HeaderMap,
    Json(request): Json<ReasonCodeRequest>,
) -> Response
where
    R: ReasonCodeStore + Clone + Send + Sync + 'static,
    A: AdminStore + Clone + Send + Sync + 'static,
    C: ClockSource + Clone + Send + Sync + 'static,
{
    let context = match require_permission(
        &state.admin,
        &state.clock,
        &headers,
        ConsolePermission::ManageReasonCodes,
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
    let Some(reason_code_id) =
        mint_ulid(state.clock.now().as_milliseconds_since_epoch()).map(ReasonCodeId::new)
    else {
        return reason_code_entropy_unavailable();
    };
    let reason_code = match build_reason_code(&request, reason_code_id) {
        Ok(reason_code) => reason_code,
        Err(refusal) => return refusal.into_response(),
    };
    match state.reason_codes.create(tenant_id, &reason_code).await {
        Ok(CreateOutcome::Created(version)) => {
            audit_action(
                &state.audit,
                &state.clock,
                &context,
                Some(tenant_id),
                "reason_codes.create",
                "reason_code",
                &reason_code_id.to_string(),
                None,
                serde_json::to_value(ReasonCodeAuditSummary::of(&reason_code)).ok(),
            )
            .await;
            versioned_created(reason_code, &version)
        }
        // Unreachable while the id is minted here rather than sent by the caller, and answered
        // anyway — the seam cannot know that, and an entry historic events cite must never be
        // silently replaced.
        Ok(CreateOutcome::AlreadyExists) => already_exists("reason code"),
        Err(error) => reason_code_error_response(&error),
    }
}

#[utoipa::path(
    put,
    path = "/admin/reason-codes/{reason_code_id}",
    params(
        ("reason_code_id" = String, Path, description = "The entry's 26-character ULID"),
        ("If-Match" = String, Header, description = "The ETag the entry was read at \
                                                     (ADR-0094). Required; a wildcard is refused"),
    ),
    request_body(
        description = "The entry's fields as they should now stand, including `active` — retiring \
                       and restoring are this route, not a lifecycle route of their own, so they \
                       go through the same version check as any other edit",
        content_type = "application/json",
    ),
    responses(
        (status = 200, description = "Updated. The body is the entry as stored and the ETag \
                                      carries its new version"),
        (status = 400, description = "A field is blank or unknown, or If-Match is absent, \
                                      malformed, or a wildcard", body = crate::openapi_admin::ErrorResponse),
        (status = 401, description = "No session", body = crate::openapi_admin::ErrorResponse),
        (status = 403, description = "The role lacks console.reason_codes.manage", body = crate::openapi_admin::ErrorResponse),
        (status = 404, description = "The tenant has no entry with that id", body = crate::openapi_admin::ErrorResponse),
        (status = 412, description = "The entry changed since you read it: re-read it and try \
                                      again", body = crate::openapi_admin::ErrorResponse),
        (status = 503, description = "The reason code store is unreachable", body = crate::openapi_admin::ErrorResponse),
    ),
    tag = "reason codes",
)]
/// A super-admin edits a reason code in place, by the path id — including retiring and restoring
/// it, which is [`PublishedReasonCode::active`] and not a route of its own.
pub(super) async fn admin_update_reason_code<R, A, C>(
    State(state): State<ReasonCodeState<R, A, C>>,
    headers: HeaderMap,
    Path(reason_code_id): Path<String>,
    Json(request): Json<ReasonCodeRequest>,
) -> Response
where
    R: ReasonCodeStore + Clone + Send + Sync + 'static,
    A: AdminStore + Clone + Send + Sync + 'static,
    C: ClockSource + Clone + Send + Sync + 'static,
{
    let context = match require_permission(
        &state.admin,
        &state.clock,
        &headers,
        ConsolePermission::ManageReasonCodes,
    )
    .await
    {
        Ok(context) => context,
        Err(denied) => return denied,
    };
    let (tenant_id, reason_code_id) = match parse_ulid_fields([
        ("tenant_id", &request.tenant_id),
        ("reason_code_id", &reason_code_id),
    ]) {
        Ok([tenant_id, reason_code_id]) => {
            (TenantId::new(tenant_id), ReasonCodeId::new(reason_code_id))
        }
        Err(refusal) => return refusal,
    };
    let before = match state.reason_codes.get(tenant_id, reason_code_id).await {
        Ok(row) => row.map(|row| row.record),
        Err(error) => return reason_code_error_response(&error),
    };
    let Some(before) = before else {
        return not_found("reason code");
    };
    let reason_code = match build_reason_code(&request, reason_code_id) {
        Ok(reason_code) => reason_code,
        Err(refusal) => return refusal.into_response(),
    };
    let expected = match if_match(&headers) {
        Ok(expected) => expected,
        Err(refusal) => return refusal,
    };
    match state
        .reason_codes
        .update(tenant_id, &reason_code, &expected)
        .await
    {
        Ok(UpdateOutcome::Updated(version)) => {
            audit_action(
                &state.audit,
                &state.clock,
                &context,
                Some(tenant_id),
                "reason_codes.update",
                "reason_code",
                &reason_code_id.to_string(),
                serde_json::to_value(ReasonCodeAuditSummary::of(&before)).ok(),
                serde_json::to_value(ReasonCodeAuditSummary::of(&reason_code)).ok(),
            )
            .await;
            versioned_ok(reason_code, &version)
        }
        Ok(UpdateOutcome::VersionMismatch) => version_mismatch(),
        Ok(UpdateOutcome::NotFound) => not_found("reason code"),
        Err(error) => reason_code_error_response(&error),
    }
}

#[utoipa::path(
    delete,
    path = "/admin/reason-codes/{reason_code_id}",
    params(
        ("reason_code_id" = String, Path, description = "The entry's 26-character ULID"),
        ("tenant_id" = String, Query, description = "The tenant that owns it (a 26-character ULID)"),
    ),
    responses(
        (status = 204, description = "Deleted. Prefer retiring — `PUT` with `active: false` — for \
                                      anything an event may already cite"),
        (status = 400, description = "tenant_id or reason_code_id is absent or not a ULID", body = crate::openapi_admin::ErrorResponse),
        (status = 401, description = "No session", body = crate::openapi_admin::ErrorResponse),
        (status = 403, description = "The role lacks console.reason_codes.manage", body = crate::openapi_admin::ErrorResponse),
        (status = 404, description = "The tenant has no entry with that id", body = crate::openapi_admin::ErrorResponse),
        (status = 503, description = "The reason code store is unreachable", body = crate::openapi_admin::ErrorResponse),
    ),
    tag = "reason codes",
)]
/// A super-admin deletes a reason code by id.
///
/// For the entry created by mistake. The trail keeps the whole summary as the `before`, so a
/// deletion is still readable afterwards — which is the only thing that can be done for an id an
/// event may already name.
pub(super) async fn admin_delete_reason_code<R, A, C>(
    State(state): State<ReasonCodeState<R, A, C>>,
    headers: HeaderMap,
    Path(reason_code_id): Path<String>,
    Query(query): Query<RegistryTenantQuery>,
) -> Response
where
    R: ReasonCodeStore + Clone + Send + Sync + 'static,
    A: AdminStore + Clone + Send + Sync + 'static,
    C: ClockSource + Clone + Send + Sync + 'static,
{
    let context = match require_permission(
        &state.admin,
        &state.clock,
        &headers,
        ConsolePermission::ManageReasonCodes,
    )
    .await
    {
        Ok(context) => context,
        Err(denied) => return denied,
    };
    let (tenant_id, reason_code_id) = match parse_ulid_fields([
        ("tenant_id", &query.tenant_id),
        ("reason_code_id", &reason_code_id),
    ]) {
        Ok([tenant_id, reason_code_id]) => {
            (TenantId::new(tenant_id), ReasonCodeId::new(reason_code_id))
        }
        Err(refusal) => return refusal,
    };
    let before = match state.reason_codes.get(tenant_id, reason_code_id).await {
        Ok(row) => row.map(|row| row.record),
        Err(error) => return reason_code_error_response(&error),
    };
    let Some(before) = before else {
        return not_found("reason code");
    };
    match state.reason_codes.delete(tenant_id, reason_code_id).await {
        Ok(()) => {
            audit_action(
                &state.audit,
                &state.clock,
                &context,
                Some(tenant_id),
                "reason_codes.delete",
                "reason_code",
                &reason_code_id.to_string(),
                serde_json::to_value(ReasonCodeAuditSummary::of(&before)).ok(),
                None,
            )
            .await;
            StatusCode::NO_CONTENT.into_response()
        }
        Err(error) => reason_code_error_response(&error),
    }
}
