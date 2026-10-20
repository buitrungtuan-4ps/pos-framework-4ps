// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! `/admin/catalog/courses`: list, create and update a tenant's courses.

use axum::Json;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use pos_cloud_api::console::{audit_action, mint_ulid, require_permission};
use pos_cloud_api::response::{api_error_with_details, parse_ulid_fields};
use pos_proto::ErrorStatus;
use pos_proto::determinism::ClockSource;
use pos_proto::ids::{CourseId, TenantId};

use super::{CatalogState, catalog_entropy_unavailable, catalog_error_response};
use crate::auth::admin::AdminStore;
use crate::auth::console_rbac::ConsolePermission;
use crate::catalog::{CatalogStore, Course};
use crate::registry::EntityStatus;
use crate::routes::{
    CreateCourseRequest, RegistryTenantQuery, UpdateCourseRequest, entity_status_refusal, if_match,
    not_found, parse_entity_status, version_mismatch, versioned_created, versioned_ok,
};
use crate::version::UpdateOutcome;

#[utoipa::path(
    get,
    path = "/admin/catalog/courses",
    security(("session_cookie" = [])),
    params(
        ("tenant_id" = String, Query, description = "The tenant whose courses to list (a 26-character ULID)"),
    ),
    responses(
        (status = 200, description = "The tenant's courses, as an array, each with its `etag`, in service order: by `sort` ascending, then by id (ADR-0130)"),
        (status = 400, description = "tenant_id is absent or not a ULID", body = pos_cloud_api::error_schema::ErrorResponse),
        (status = 401, description = "No session", body = pos_cloud_api::error_schema::ErrorResponse),
        (status = 403, description = "The role lacks console.data.read", body = pos_cloud_api::error_schema::ErrorResponse),
        (status = 503, description = "The catalog store is unreachable", body = pos_cloud_api::error_schema::ErrorResponse),
    ),
    tag = "catalog taxonomy",
)]
/// A super-admin lists a tenant's courses, **in service order**
/// ([ADR-0130](../../../docs/adr/0130-a-course-is-something-the-catalog-names.md)).
pub(super) async fn admin_list_courses<Cat, A, C>(
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
    match state.catalog.list_courses(tenant_id).await {
        Ok(rows) => (StatusCode::OK, Json(rows)).into_response(),
        Err(error) => catalog_error_response(&error),
    }
}

#[utoipa::path(
    post,
    path = "/admin/catalog/courses",
    security(("session_cookie" = [])),
    request_body(
        description = "`tenant_id`, the course's `name`, and `sort`, where it falls in the service sequence, ascending (absent is 0)",
        content_type = "application/json",
    ),
    responses(
        (status = 201, description = "Created, active, with the id minted here. The body is the \
                                      record and the ETag carries its version. Audited"),
        (status = 400, description = "tenant_id is absent or not a ULID, or name is blank", body = pos_cloud_api::error_schema::ErrorResponse),
        (status = 401, description = "No session", body = pos_cloud_api::error_schema::ErrorResponse),
        (status = 403, description = "The role lacks console.catalog.manage", body = pos_cloud_api::error_schema::ErrorResponse),
        (status = 503, description = "The catalog store or the entropy source is unavailable", body = pos_cloud_api::error_schema::ErrorResponse),
    ),
    tag = "catalog taxonomy",
)]
/// A super-admin creates a course; the id is minted here and returned once.
pub(super) async fn admin_create_course<Cat, A, C>(
    State(state): State<CatalogState<Cat, A, C>>,
    headers: HeaderMap,
    Json(request): Json<CreateCourseRequest>,
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
    if request.name.trim().is_empty() {
        return api_error_with_details(
            ErrorStatus::InvalidArgument,
            "name is required",
            &[("name", "REQUIRED")],
        );
    }
    let Some(course_id) =
        mint_ulid(state.clock.now().as_milliseconds_since_epoch()).map(CourseId::new)
    else {
        return catalog_entropy_unavailable();
    };
    let record = Course {
        course_id,
        tenant_id,
        name: request.name,
        sort: request.sort,
        status: EntityStatus::Active,
    };
    match state.catalog.create_course(&record).await {
        Ok(version) => {
            audit_action(
                &state.audit,
                &state.clock,
                &context,
                Some(record.tenant_id),
                "course.create",
                "course",
                &record.course_id.to_string(),
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
    path = "/admin/catalog/courses/{course_id}",
    security(("session_cookie" = [])),
    params(
        ("course_id" = String, Path, description = "The course (a 26-character ULID)"),
        ("If-Match" = String, Header, description = "The ETag the record was read at (ADR-0094). \
                                                     Required; a wildcard is refused"),
    ),
    request_body(
        description = "`tenant_id`, and the course's `name`, `sort` and `status` (`active` or `archived`) as they should now stand",
        content_type = "application/json",
    ),
    responses(
        (status = 200, description = "Updated. The body is the record and the ETag carries its new \
                                      version. Audited"),
        (status = 400, description = "course_id or tenant_id is not a ULID, name is blank, status is not `active` or `archived`, or If-Match is \
                                      absent, malformed, or a wildcard", body = pos_cloud_api::error_schema::ErrorResponse),
        (status = 401, description = "No session", body = pos_cloud_api::error_schema::ErrorResponse),
        (status = 403, description = "The role lacks console.catalog.manage", body = pos_cloud_api::error_schema::ErrorResponse),
        (status = 404, description = "The tenant has no course with that id", body = pos_cloud_api::error_schema::ErrorResponse),
        (status = 412, description = "The course changed since you read it: re-read it and try \
                                      again", body = pos_cloud_api::error_schema::ErrorResponse),
        (status = 503, description = "The catalog store is unreachable", body = pos_cloud_api::error_schema::ErrorResponse),
    ),
    tag = "catalog taxonomy",
)]
/// A super-admin renames a course, moves it in the service sequence and/or archives it.
pub(super) async fn admin_update_course<Cat, A, C>(
    State(state): State<CatalogState<Cat, A, C>>,
    headers: HeaderMap,
    Path(course_id): Path<String>,
    Json(request): Json<UpdateCourseRequest>,
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
    let (course_id, tenant_id) =
        match parse_ulid_fields([("course_id", &course_id), ("tenant_id", &request.tenant_id)]) {
            Ok([course_id, tenant_id]) => (CourseId::new(course_id), TenantId::new(tenant_id)),
            Err(refusal) => return refusal,
        };
    if request.name.trim().is_empty() {
        return api_error_with_details(
            ErrorStatus::InvalidArgument,
            "name is required",
            &[("name", "REQUIRED")],
        );
    }
    let Some(status) = parse_entity_status(&request.status) else {
        return entity_status_refusal();
    };
    let record = Course {
        course_id,
        tenant_id,
        name: request.name,
        sort: request.sort,
        status,
    };
    let expected = match if_match(&headers) {
        Ok(expected) => expected,
        Err(refusal) => return refusal,
    };
    match state.catalog.update_course(&record, &expected).await {
        Ok(UpdateOutcome::Updated(version)) => {
            audit_action(
                &state.audit,
                &state.clock,
                &context,
                Some(record.tenant_id),
                "course.update",
                "course",
                &record.course_id.to_string(),
                None,
                serde_json::to_value(&record).ok(),
            )
            .await;
            versioned_ok(record, &version)
        }
        Ok(UpdateOutcome::VersionMismatch) => version_mismatch(),
        Ok(UpdateOutcome::NotFound) => not_found("course"),
        Err(error) => catalog_error_response(&error),
    }
}
