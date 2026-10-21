// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! `/admin/employees`: a tenant's employees.

use std::collections::BTreeSet;

use axum::Json;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use pos_cloud_api::console::{audit_action, mint_ulid, require_permission};
use pos_cloud_api::response::{api_error_with_details, parse_ulid_fields};
use pos_proto::ErrorStatus;
use pos_proto::determinism::ClockSource;
use pos_proto::ids::{StoreId, TenantId};

use super::PeopleState;
use super::requests::{
    CreateEmployeeRequest, EmployeeListQuery, PIN_MAX_DIGITS, PeopleTenantQuery, SetPinRequest,
    UpdateEmployeeRequest, hash_pin, people_entropy_unavailable, people_error_response,
    pin_is_well_formed,
};
use crate::auth::admin::{AdminContext, AdminStore};
use crate::auth::console_rbac::ConsolePermission;
use crate::config_tree::ConfigTreeStore;
use crate::people::{
    AssignmentStore, Employee, EmployeeId, EmployeeListFilter, EmployeeSort, EmployeeStore,
    EmployeeStoreError, EmployeeUpdate, NewEmployee, RoleTemplateStore,
};
use crate::registry::{EntityStatus, RegistryStore};
use crate::version::{UpdateOutcome, Version};
use pos_cloud_api::conditional::{if_match, version_mismatch, versioned_ok, with_etag};
use pos_cloud_api::paged::{paged_ok, parse_page};
use pos_cloud_api::refusal::{enum_refusal, not_found, present_param};
use pos_cloud_catalog::routes::catalog::page_shaping_needs_a_limit_refusal;
use pos_cloud_config::route::{entity_status_refusal, parse_entity_status};

#[utoipa::path(
    get,
    path = "/admin/employees",
    security(("session_cookie" = [])),
    extensions(("x-permission" = json!("console.people.read"))),
    params(
        ("tenant_id" = String, Query, description = "The tenant whose employees to list (a 26-character ULID)"),
        ("limit" = Option<u32>, Query, description = "How many employees to return, 1 to 500. \
                                                    Absent, the whole roster is returned as an array \
                                                    rather than a page"),
        ("offset" = Option<u32>, Query, description = "How many employees to skip, 0 to 100000. Only \
                                                     with `limit`"),
        ("q" = Option<String>, Query, description = "A case-insensitive substring the person's name \
                                                   or staff code must contain. Only with `limit`"),
        ("sort" = Option<String>, Query, description = "`newest` (the default), `name` or `code`. \
                                                      Only with `limit`"),
        ("order" = Option<String>, Query, description = "`asc` (the default) or `desc`, inverting \
                                                       `sort`'s direction. Only with `limit`"),
    ),
    responses(
        (status = 200, description = "Without `limit`, every employee as an array; with it, a page: \
                                      `items`, `total`, `limit` and `offset`. Each employee has \
                                      `employee_id`, `tenant_id`, `code`, `name`, `status` and \
                                      `has_pin` — never the PIN or its hash"),
        (status = 400, description = "tenant_id is absent or not a ULID; limit or offset is not an \
                                      integer in range; offset, q, sort or order is sent without \
                                      limit; or sort or order is not one the route accepts", body = pos_cloud_api::error_schema::ErrorResponse),
        (status = 401, description = "No session", body = pos_cloud_api::error_schema::ErrorResponse),
        (status = 403, description = "The role lacks console.people.read", body = pos_cloud_api::error_schema::ErrorResponse),
        (status = 503, description = "The people store is unreachable", body = pos_cloud_api::error_schema::ErrorResponse),
    ),
    tag = "people",
)]
/// A super-admin lists a tenant's employees. Behind [`ConsolePermission::ReadPeople`]: every row
/// carries a person's name and staff code.
pub(super) async fn admin_list_employees<P, R, Cfg, A, C>(
    State(state): State<PeopleState<P, R, Cfg, A, C>>,
    headers: HeaderMap,
    Query(query): Query<EmployeeListQuery>,
) -> Response
where
    P: EmployeeStore + Clone + Send + Sync + 'static,
    R: Clone + Send + Sync + 'static,
    Cfg: Clone + Send + Sync + 'static,
    A: AdminStore + Clone + Send + Sync + 'static,
    C: ClockSource + Clone + Send + Sync + 'static,
{
    if let Err(denied) = require_permission(
        &state.admin,
        &state.clock,
        &headers,
        ConsolePermission::ReadPeople,
    )
    .await
    {
        return denied;
    }
    let tenant_id = match parse_ulid_fields([("tenant_id", &query.tenant_id)]) {
        Ok([tenant_id]) => TenantId::new(tenant_id),
        Err(refusal) => return refusal,
    };
    // Two reads, chosen by whether the caller named a limit. The unpaged one is not legacy: the
    // permission node is compiled from the whole roster, and a node built from a page would be
    // missing whoever fell off it (ADR-0098). The console's table is what wants a page — and gets
    // strictly less T1 data per response than the read beside it (ADR-0070).
    let Some(page) = parse_page(query.limit.as_deref(), query.offset.as_deref()) else {
        // `q`/`sort`/`order` shape a *page*. Honouring them on the whole-roster read would answer a
        // different question than the caller asked; ignoring them would answer the wrong one
        // silently. The route names the missing parameter instead.
        for (field, value) in [
            ("q", query.q.as_deref()),
            ("sort", query.sort.as_deref()),
            ("order", query.order.as_deref()),
        ] {
            if present_param(value).is_some() {
                return page_shaping_needs_a_limit_refusal(field);
            }
        }
        return match state.people.list(tenant_id).await {
            Ok(employees) => (StatusCode::OK, Json(employees)).into_response(),
            Err(error) => people_error_response(&error),
        };
    };
    let page = match page {
        Ok(page) => page,
        Err(refusal) => return refusal,
    };
    let filter = match employee_list_filter(
        query.q.as_deref(),
        query.sort.as_deref(),
        query.order.as_deref(),
    ) {
        Ok(filter) => filter,
        Err(refusal) => return refusal,
    };
    match state.people.list_page(tenant_id, page, &filter).await {
        Ok(read) => paged_ok(read, page),
        Err(error) => people_error_response(&error),
    }
}

/// Reads `?q=`, `?sort=` and `?order=` into an [`EmployeeListFilter`], refusing a token outside the
/// route's own closed sets.
///
/// The refusal names the field and lists what it accepts (ADR-0096's shape), so a caller that sent
/// `?sort=hired` is told `sort` must be `newest`, `name` or `code` rather than being handed the
/// default order and left to wonder why nothing moved.
#[expect(
    clippy::result_large_err,
    reason = "the Err is an axum Response by design — it *is* the 400 the route returns, the same \
              shape `item_list_filter` and the other route helpers carry"
)]
fn employee_list_filter(
    search: Option<&str>,
    sort: Option<&str>,
    order: Option<&str>,
) -> Result<EmployeeListFilter, Response> {
    let search = present_param(search).map(str::to_owned);
    let sort = match present_param(sort) {
        None => EmployeeSort::default(),
        Some(token) => match EmployeeSort::from_token(token) {
            Some(sort) => sort,
            None => {
                return Err(enum_refusal("sort", EmployeeSort::tokens().iter().copied()));
            }
        },
    };
    let descending = match present_param(order) {
        // Absent is ascending — the same reading `?order=asc` has, so a caller that omits the
        // parameter and one that spells out the default get the same page.
        None | Some("asc") => false,
        Some("desc") => true,
        Some(_unknown) => return Err(enum_refusal("order", ["asc", "desc"])),
    };
    Ok(EmployeeListFilter {
        search,
        sort,
        descending,
    })
}

#[utoipa::path(
    get,
    path = "/admin/employees/{employee_id}",
    security(("session_cookie" = [])),
    extensions(("x-permission" = json!("console.people.read"))),
    params(
        ("employee_id" = String, Path, description = "The employee's 26-character ULID"),
        ("tenant_id" = String, Query, description = "The tenant that employs them (a 26-character ULID)"),
    ),
    responses(
        (status = 200, description = "The employee, with an ETag carrying the version it was read at \
                                      — the token a PATCH must send back as If-Match"),
        (status = 400, description = "tenant_id or employee_id is absent or not a ULID", body = pos_cloud_api::error_schema::ErrorResponse),
        (status = 401, description = "No session", body = pos_cloud_api::error_schema::ErrorResponse),
        (status = 403, description = "The role lacks console.people.read", body = pos_cloud_api::error_schema::ErrorResponse),
        (status = 404, description = "The tenant has no employee with that id", body = pos_cloud_api::error_schema::ErrorResponse),
        (status = 503, description = "The people store is unreachable", body = pos_cloud_api::error_schema::ErrorResponse),
    ),
    tag = "people",
)]
/// A super-admin reads one employee within its tenant. Behind [`ConsolePermission::ReadPeople`],
/// like the list.
pub(super) async fn admin_get_employee<P, R, Cfg, A, C>(
    State(state): State<PeopleState<P, R, Cfg, A, C>>,
    headers: HeaderMap,
    Path(employee_id): Path<String>,
    Query(query): Query<PeopleTenantQuery>,
) -> Response
where
    P: EmployeeStore + Clone + Send + Sync + 'static,
    R: Clone + Send + Sync + 'static,
    Cfg: Clone + Send + Sync + 'static,
    A: AdminStore + Clone + Send + Sync + 'static,
    C: ClockSource + Clone + Send + Sync + 'static,
{
    if let Err(denied) = require_permission(
        &state.admin,
        &state.clock,
        &headers,
        ConsolePermission::ReadPeople,
    )
    .await
    {
        return denied;
    }
    let (tenant_id, employee_id) = match parse_ulid_fields([
        ("tenant_id", &query.tenant_id),
        ("employee_id", &employee_id),
    ]) {
        Ok([tenant_id, employee_id]) => (TenantId::new(tenant_id), EmployeeId::new(employee_id)),
        Err(refusal) => return refusal,
    };
    match state.people.get(tenant_id, employee_id).await {
        Ok(Some(employee)) => versioned_ok(employee.record, &employee.etag),
        Ok(None) => not_found("employee"),
        Err(error) => people_error_response(&error),
    }
}

#[utoipa::path(
    post,
    path = "/admin/employees",
    security(("session_cookie" = [])),
    extensions(("x-permission" = json!("console.people.manage"))),
    request_body(
        description = "`tenant_id`, the staff `code` the person types to sign in, unique within the \
                       tenant, and their `name`. No PIN: it is set with `PUT …/pin`",
        content_type = "application/json",
    ),
    responses(
        (status = 201, description = "Created. The body is `{ \"id\": … }` and the ETag carries \
                                      the new employee's version. The employee starts active"),
        (status = 400, description = "tenant_id is absent or not a ULID, or code or name is blank", body = pos_cloud_api::error_schema::ErrorResponse),
        (status = 401, description = "No session", body = pos_cloud_api::error_schema::ErrorResponse),
        (status = 403, description = "The role lacks console.people.manage", body = pos_cloud_api::error_schema::ErrorResponse),
        (status = 409, description = "The tenant already uses that staff code. The error names the \
                                      `code` field, never the code", body = pos_cloud_api::error_schema::ErrorResponse),
        (status = 503, description = "The people store or the entropy source is unavailable", body = pos_cloud_api::error_schema::ErrorResponse),
    ),
    tag = "people",
)]
/// A super-admin creates an employee (no PIN yet). Audited `employee.create` with id/code/status —
/// never the name. A staff code the tenant already uses is `409` ([`code_in_use`]), and nothing is
/// written or audited.
pub(super) async fn admin_create_employee<P, R, Cfg, A, C>(
    State(state): State<PeopleState<P, R, Cfg, A, C>>,
    headers: HeaderMap,
    Json(request): Json<CreateEmployeeRequest>,
) -> Response
where
    P: EmployeeStore + Clone + Send + Sync + 'static,
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
    // Two required fields, so name the ones actually blank rather than both — the same per-field
    // rule the ULID and opening-hours refusals follow.
    let mut missing: Vec<(&str, &str)> = Vec::with_capacity(2);
    if request.code.trim().is_empty() {
        missing.push(("code", "REQUIRED"));
    }
    if request.name.trim().is_empty() {
        missing.push(("name", "REQUIRED"));
    }
    if !missing.is_empty() {
        return api_error_with_details(
            ErrorStatus::InvalidArgument,
            "code and name are required",
            &missing,
        );
    }
    let Some(employee_id) =
        mint_ulid(state.clock.now().as_milliseconds_since_epoch()).map(EmployeeId::new)
    else {
        return people_entropy_unavailable();
    };
    let new_employee = NewEmployee {
        employee_id,
        tenant_id,
        code: request.code.clone(),
        name: request.name,
    };
    match state.people.create(&new_employee).await {
        Ok(version) => {
            // The trail records id/code/status — never the name (ADR-0070).
            let after = serde_json::json!({
                "id": employee_id.to_string(),
                "code": request.code,
                "status": EntityStatus::Active.as_str(),
            });
            audit_action(
                &state.audit,
                &state.clock,
                &context,
                Some(tenant_id),
                "employee.create",
                "employee",
                &employee_id.to_string(),
                None,
                Some(after),
            )
            .await;
            with_etag(
                (
                    StatusCode::CREATED,
                    Json(serde_json::json!({ "id": employee_id.to_string() })),
                )
                    .into_response(),
                &version,
            )
        }
        Err(EmployeeStoreError::CodeInUse) => code_in_use(),
        Err(error) => people_error_response(&error),
    }
}

/// The `409` a new employee earns for a staff code the tenant already uses: a code is unique within a
/// tenant, because it is what a person types to sign in. It names the request's `code` field and
/// never the code itself, which identifies a person.
///
/// A conflict rather than an outage: before the store said which, this was the people service's
/// `503`, which sent the operator to retry what can never succeed. A code cannot be changed once
/// set, so only a create meets this.
fn code_in_use() -> Response {
    api_error_with_details(
        ErrorStatus::AlreadyExists,
        "that staff code is already in use: choose another code",
        &[("code", "ALREADY_EXISTS")],
    )
}

#[utoipa::path(
    patch,
    path = "/admin/employees/{employee_id}",
    security(("session_cookie" = [])),
    extensions(("x-permission" = json!("console.people.manage"))),
    params(
        ("employee_id" = String, Path, description = "The employee's 26-character ULID"),
        ("If-Match" = String, Header, description = "The ETag the employee was read at \
                                                     (ADR-0094). Required; a wildcard is refused"),
    ),
    request_body(
        description = "`tenant_id`, the `name` and the `status` (`active` or `archived`) as they \
                       should now stand. The staff code cannot be changed",
        content_type = "application/json",
    ),
    responses(
        (status = 200, description = "Archived. The person is taken off every store they were \
                                      assigned to at once (ADR-0158 decision 7): `stores` lists each \
                                      store with its `outcome`, `PERMISSIONS_PUBLISH_APPLIED` with \
                                      its `config_version_id` or `PERMISSIONS_PUBLISH_FAILED`, which \
                                      `POST /admin/people/publish` retries. The ETag carries the new \
                                      version"),
        (status = 204, description = "Updated: a rename, or restoring an archived person. Nothing is \
                                      published until someone publishes. The ETag carries the new \
                                      version"),
        (status = 400, description = "tenant_id or employee_id is absent or not a ULID, name is \
                                      blank, status is not `active` or `archived`, or If-Match is \
                                      absent, malformed, or a wildcard", body = pos_cloud_api::error_schema::ErrorResponse),
        (status = 401, description = "No session", body = pos_cloud_api::error_schema::ErrorResponse),
        (status = 403, description = "The role lacks console.people.manage", body = pos_cloud_api::error_schema::ErrorResponse),
        (status = 404, description = "The tenant has no employee with that id", body = pos_cloud_api::error_schema::ErrorResponse),
        (status = 412, description = "The employee changed since you read it: re-read them and try \
                                      again", body = pos_cloud_api::error_schema::ErrorResponse),
        (status = 503, description = "The people store, the registry or the store groups is \
                                      unreachable", body = pos_cloud_api::error_schema::ErrorResponse),
    ),
    tag = "people",
)]
/// A super-admin renames an employee and/or sets their status. Audited `employee.update` with
/// before/after id/code/status — never the name.
///
/// Archiving the person publishes the `permissions` node at once to every store they are assigned
/// to, so they cannot sign in at any of them a moment longer
/// ([ADR-0158](../../../docs/adr/0158-the-till-enforces-each-persons-own-permissions.md)
/// decision 7), and the answer is then `200` with each store's outcome. Any other update — a
/// rename, or restoring an archived person — publishes as before, and answers `204`.
pub(super) async fn admin_update_employee<P, R, Cfg, A, C>(
    State(state): State<PeopleState<P, R, Cfg, A, C>>,
    headers: HeaderMap,
    Path(employee_id): Path<String>,
    Json(request): Json<UpdateEmployeeRequest>,
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
    let (tenant_id, employee_id, status) = match read_employee_update(&employee_id, &request) {
        Ok(read) => read,
        Err(refusal) => return refusal,
    };
    // Read the current row for the audit `before` (id/code/status, never the name) and to answer 404.
    let existing = match EmployeeStore::get(&state.people, tenant_id, employee_id).await {
        Ok(Some(employee)) => employee,
        Ok(None) => return not_found("employee"),
        Err(error) => return people_error_response(&error),
    };
    let update = EmployeeUpdate {
        employee_id,
        tenant_id,
        name: request.name,
        status,
    };
    let reached = match stores_told_of_archive(&state, &update, existing.record.status).await {
        Ok(reached) => reached,
        Err(refusal) => return refusal,
    };
    let version = match write_employee_update(&state, &headers, &update).await {
        Ok(version) => version,
        Err(refusal) => return refusal,
    };
    record_employee_update(&state, &context, &update, &existing.record).await;
    let cause = ("employee_id", employee_id.to_string());
    state
        .answer_update(&context, tenant_id, &version, reached, cause)
        .await
}

/// The ids, the name and the status an update names, refused in that order.
#[expect(
    clippy::result_large_err,
    reason = "the Err is an axum Response by design — it is the 400 the caller returns"
)]
fn read_employee_update(
    employee_id: &str,
    request: &UpdateEmployeeRequest,
) -> Result<(TenantId, EmployeeId, EntityStatus), Response> {
    let [tenant_id, employee_id] = parse_ulid_fields([
        ("tenant_id", &request.tenant_id),
        ("employee_id", employee_id),
    ])?;
    if request.name.trim().is_empty() {
        return Err(api_error_with_details(
            ErrorStatus::InvalidArgument,
            "name is required",
            &[("name", "REQUIRED")],
        ));
    }
    let Some(status) = parse_entity_status(&request.status) else {
        return Err(entity_status_refusal());
    };
    Ok((
        TenantId::new(tenant_id),
        EmployeeId::new(employee_id),
        status,
    ))
}

/// When the update archives the person, the stores to tell, read before the write, so a failed
/// read refuses a write that has not happened rather than leaving one done with nobody told: every
/// store any of their assignments reaches, through a group or the tenant as well. `None` when it
/// archives nobody.
async fn stores_told_of_archive<P, R, Cfg, A, C>(
    state: &PeopleState<P, R, Cfg, A, C>,
    update: &EmployeeUpdate,
    was: EntityStatus,
) -> Result<Option<BTreeSet<StoreId>>, Response>
where
    P: AssignmentStore,
    R: RegistryStore,
{
    if was == EntityStatus::Archived || update.status != EntityStatus::Archived {
        return Ok(None);
    }
    let assignments =
        AssignmentStore::list_for_employee(&state.people, update.tenant_id, update.employee_id)
            .await
            .map_err(|error| people_error_response(&error))?;
    let scopes = assignments.iter().map(|assignment| assignment.scope);
    state
        .stores_reached(update.tenant_id, scopes)
        .await
        .map(Some)
}

/// Writes `update` at the version the request's `If-Match` names, and answers the new version or
/// the refusal: a missing or malformed `If-Match`, a stale version, or a person gone since the read.
async fn write_employee_update<P, R, Cfg, A, C>(
    state: &PeopleState<P, R, Cfg, A, C>,
    headers: &HeaderMap,
    update: &EmployeeUpdate,
) -> Result<Version, Response>
where
    P: EmployeeStore,
{
    let expected = if_match(headers)?;
    match EmployeeStore::update(&state.people, update, &expected).await {
        Ok(UpdateOutcome::Updated(version)) => Ok(version),
        Ok(UpdateOutcome::VersionMismatch) => Err(version_mismatch()),
        Ok(UpdateOutcome::NotFound) => Err(not_found("employee")),
        Err(error) => Err(people_error_response(&error)),
    }
}

/// Records `employee.update` with the id, code and status before and after — never the name.
async fn record_employee_update<P, R, Cfg, A, C>(
    state: &PeopleState<P, R, Cfg, A, C>,
    context: &AdminContext,
    update: &EmployeeUpdate,
    existing: &Employee,
) where
    C: ClockSource,
{
    let before = serde_json::json!({
        "id": update.employee_id.to_string(),
        "code": existing.code,
        "status": existing.status.as_str(),
    });
    let after = serde_json::json!({
        "id": update.employee_id.to_string(),
        "code": existing.code,
        "status": update.status.as_str(),
    });
    audit_action(
        &state.audit,
        &state.clock,
        context,
        Some(update.tenant_id),
        "employee.update",
        "employee",
        &update.employee_id.to_string(),
        Some(before),
        Some(after),
    )
    .await;
}

#[utoipa::path(
    put,
    path = "/admin/employees/{employee_id}/pin",
    security(("session_cookie" = [])),
    extensions(("x-permission" = json!("console.people.manage"))),
    params(
        ("employee_id" = String, Path, description = "The employee's 26-character ULID"),
    ),
    request_body(
        description = "`tenant_id` and the `pin`: digits only, at least the tenant's \
                       `session.pin_min_length` (four where it sets none) and at most eight. The PIN \
                       is hashed at once and is never returned, stored in the clear, logged or \
                       audited (ADR-0070)",
        content_type = "application/json",
    ),
    responses(
        (status = 204, description = "Set. A store learns the new PIN when its people are next \
                                      published"),
        (status = 400, description = "tenant_id or employee_id is absent or not a ULID, or the PIN \
                                      is not digits of an allowed length", body = pos_cloud_api::error_schema::ErrorResponse),
        (status = 401, description = "No session", body = pos_cloud_api::error_schema::ErrorResponse),
        (status = 403, description = "The role lacks console.people.manage", body = pos_cloud_api::error_schema::ErrorResponse),
        (status = 404, description = "The tenant has no employee with that id", body = pos_cloud_api::error_schema::ErrorResponse),
        (status = 503, description = "The people store, the settings store or the entropy source is \
                                      unavailable", body = pos_cloud_api::error_schema::ErrorResponse),
    ),
    tag = "people",
)]
/// A super-admin sets or resets an employee's PIN. The digits are hashed with Argon2id here and never
/// returned, stored raw, or **audited** — the entry records only that a PIN was set, by whom
/// (ADR-0070).
///
/// A PIN has at least the digits the tenant's `session.pin_min_length` asks for, four where it asks
/// for none, and at most eight (ADR-0160). This is the one place the minimum is applied: the edge
/// holds only a PIN's hash. Raising the minimum leaves a PIN already set as it is. If the tenant's
/// settings cannot be read, the PIN is refused rather than held to four.
pub(super) async fn admin_set_employee_pin<P, R, Cfg, A, C>(
    State(state): State<PeopleState<P, R, Cfg, A, C>>,
    headers: HeaderMap,
    Path(employee_id): Path<String>,
    Json(request): Json<SetPinRequest>,
) -> Response
where
    P: EmployeeStore + Clone + Send + Sync + 'static,
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
    let (tenant_id, employee_id) = match parse_ulid_fields([
        ("tenant_id", &request.tenant_id),
        ("employee_id", &employee_id),
    ]) {
        Ok([tenant_id, employee_id]) => (TenantId::new(tenant_id), EmployeeId::new(employee_id)),
        Err(refusal) => return refusal,
    };
    let fewest = match state.pin_minimum.fewest_digits(tenant_id).await {
        Ok(fewest) => fewest,
        Err(error) => return people_error_response(&error),
    };
    if !pin_is_well_formed(&request.pin, fewest) {
        return api_error_with_details(
            ErrorStatus::InvalidArgument,
            format!("the PIN must be {fewest} to {PIN_MAX_DIGITS} digits"),
            &[("pin", "OUT_OF_RANGE")],
        );
    }
    let Some(pin_phc) = hash_pin(&request.pin) else {
        return people_entropy_unavailable();
    };
    match state.people.set_pin(tenant_id, employee_id, &pin_phc).await {
        Ok(true) => {
            // No before/after: the trail records that a PIN was set for this employee, by whom — never
            // the PIN or its hash (ADR-0070).
            audit_action(
                &state.audit,
                &state.clock,
                &context,
                Some(tenant_id),
                "employee.set_pin",
                "employee",
                &employee_id.to_string(),
                None,
                None,
            )
            .await;
            StatusCode::NO_CONTENT.into_response()
        }
        Ok(false) => not_found("employee"),
        Err(error) => people_error_response(&error),
    }
}
