// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The `/admin/packages` handlers: list and create packages, list and publish versions.

use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use pos_cloud_api::console::{audit_action, mint_ulid, require_permission};
use pos_cloud_api::response::{api_error_with_details, parse_ulid_fields};
use pos_cloud_config::packages::{
    PackageRecord, PackageStore, PackageVersionRecord, min_edge_version,
};
use pos_core::package::check_content;
use pos_proto::ErrorStatus;
use pos_proto::determinism::ClockSource;
use pos_proto::ids::{PackageId, PackageVersionId, TenantId};
use pos_proto::package::{CONTENT_BYTES_MAX, LABEL_CHARS_MAX, PackageContent};
use pos_proto::text::{DisplayName, ReleaseTag};

use super::{
    PackageRequest, PackageState, PackageVersionRequest, package_entropy_unavailable,
    package_error_response,
};
use crate::auth::admin::AdminStore;
use crate::auth::console_rbac::ConsolePermission;
use crate::http::{FieldRefusal, Json, RegistryTenantQuery, already_exists, not_found};
use crate::version::CreateOutcome;

#[utoipa::path(
    get,
    path = "/admin/packages",
    security(("session_cookie" = [])),
    extensions(("x-permission" = json!("console.data.read"))),
    params(("tenant_id" = String, Query, description = "The tenant whose packages to list (a 26-character ULID)")),
    responses(
        (status = 200, body = [PackageRecord], description = "The tenant's packages, in id order"),
        (status = 400, description = "tenant_id is absent or not a ULID", body = crate::openapi_admin::ErrorResponse),
        (status = 401, description = "No session", body = crate::openapi_admin::ErrorResponse),
        (status = 403, description = "The role lacks console.data.read", body = crate::openapi_admin::ErrorResponse),
        (status = 503, description = "The package store is unreachable", body = crate::openapi_admin::ErrorResponse),
    ),
    tag = "packages",
)]
/// A console user lists a tenant's packages.
pub(super) async fn admin_list_packages<P, A, C>(
    State(state): State<PackageState<P, A, C>>,
    headers: HeaderMap,
    Query(query): Query<RegistryTenantQuery>,
) -> Response
where
    P: PackageStore + Clone + Send + Sync + 'static,
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
    match state.packages.list_packages(tenant_id).await {
        Ok(rows) => (StatusCode::OK, Json(rows)).into_response(),
        Err(error) => package_error_response(&error),
    }
}

#[utoipa::path(
    post,
    path = "/admin/packages",
    security(("session_cookie" = [])),
    extensions(("x-permission" = json!("console.packages.manage"))),
    request_body(
        content = PackageRequest, description = "The tenant and the package's name. The id is server-minted",
        content_type = "application/json",
    ),
    responses(
        (status = 201, body = PackageRecord, description = "Created: the package as stored, with the id the server minted"),
        (status = 400, description = "tenant_id is not a ULID, or the name is blank or longer than 64 characters", body = crate::openapi_admin::ErrorResponse),
        (status = 401, description = "No session", body = crate::openapi_admin::ErrorResponse),
        (status = 403, description = "The role lacks console.packages.manage", body = crate::openapi_admin::ErrorResponse),
        (status = 409, description = "A package already holds that id", body = crate::openapi_admin::ErrorResponse),
        (status = 503, description = "The package store is unreachable, or the OS would not give the entropy the id needs", body = crate::openapi_admin::ErrorResponse),
    ),
    tag = "packages",
)]
/// A console user creates a package; the server mints its id.
pub(super) async fn admin_create_package<P, A, C>(
    State(state): State<PackageState<P, A, C>>,
    headers: HeaderMap,
    Json(request): Json<PackageRequest>,
) -> Response
where
    P: PackageStore + Clone + Send + Sync + 'static,
    A: AdminStore + Clone + Send + Sync + 'static,
    C: ClockSource + Clone + Send + Sync + 'static,
{
    let context = match require_permission(
        &state.admin,
        &state.clock,
        &headers,
        ConsolePermission::ManagePackages,
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
    let name = request.name.trim();
    if name.is_empty() || name.chars().count() > LABEL_CHARS_MAX {
        return FieldRefusal::new("a package name is 1 to 64 characters", "name", "INVALID")
            .into_response();
    }
    let now = state.clock.now();
    let Some(package_id) = mint_ulid(now.as_milliseconds_since_epoch()).map(PackageId::new) else {
        return package_entropy_unavailable();
    };
    let package = PackageRecord {
        package_id,
        name: DisplayName::new(name),
        create_time: now,
    };
    match state.packages.create_package(tenant_id, &package).await {
        Ok(CreateOutcome::Created(_)) => {
            audit_action(
                &state.audit,
                &state.clock,
                &context,
                Some(tenant_id),
                "packages.create",
                "package",
                &package_id.to_string(),
                None,
                serde_json::to_value(&package).ok(),
            )
            .await;
            (StatusCode::CREATED, Json(package)).into_response()
        }
        Ok(CreateOutcome::AlreadyExists) => already_exists("package"),
        Err(error) => package_error_response(&error),
    }
}

#[utoipa::path(
    get,
    path = "/admin/packages/{package_id}/versions",
    security(("session_cookie" = [])),
    extensions(("x-permission" = json!("console.data.read"))),
    params(
        ("package_id" = String, Path, description = "The package's 26-character ULID"),
        ("tenant_id" = String, Query, description = "The tenant that owns it (a 26-character ULID)"),
    ),
    responses(
        (status = 200, body = [PackageVersionRecord], description = "The package's published versions, in id order: publish order, but for two published in the same millisecond"),
        (status = 400, description = "tenant_id or package_id is absent or not a ULID", body = crate::openapi_admin::ErrorResponse),
        (status = 401, description = "No session", body = crate::openapi_admin::ErrorResponse),
        (status = 403, description = "The role lacks console.data.read", body = crate::openapi_admin::ErrorResponse),
        (status = 404, description = "The tenant has no package with that id", body = crate::openapi_admin::ErrorResponse),
        (status = 503, description = "The package store is unreachable", body = crate::openapi_admin::ErrorResponse),
    ),
    tag = "packages",
)]
/// A console user lists a package's published versions.
pub(super) async fn admin_list_package_versions<P, A, C>(
    State(state): State<PackageState<P, A, C>>,
    headers: HeaderMap,
    Path(package_id): Path<String>,
    Query(query): Query<RegistryTenantQuery>,
) -> Response
where
    P: PackageStore + Clone + Send + Sync + 'static,
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
    let (tenant_id, package_id) =
        match parse_ulid_fields([("tenant_id", &query.tenant_id), ("package_id", &package_id)]) {
            Ok([tenant_id, package_id]) => (TenantId::new(tenant_id), PackageId::new(package_id)),
            Err(refusal) => return refusal,
        };
    match state.packages.get_package(tenant_id, package_id).await {
        Ok(Some(_)) => {}
        Ok(None) => return not_found("package"),
        Err(error) => return package_error_response(&error),
    }
    match state.packages.list_versions(tenant_id, package_id).await {
        Ok(rows) => (StatusCode::OK, Json(rows)).into_response(),
        Err(error) => package_error_response(&error),
    }
}

#[utoipa::path(
    post,
    path = "/admin/packages/{package_id}/versions",
    security(("session_cookie" = [])),
    extensions(("x-permission" = json!("console.packages.manage"))),
    params(("package_id" = String, Path, description = "The package's 26-character ULID")),
    request_body(
        content = PackageVersionRequest, description = "The tenant, the version's label and its time slots. The \
                       version id and the minimum edge release are the server's",
        content_type = "application/json",
    ),
    responses(
        (status = 201, body = PackageVersionRecord, description = "Published: the version as stored, for good. Its content \
                                      carries the id the server minted and the oldest edge \
                                      release that runs it"),
        (status = 400, description = "tenant_id or package_id is not a ULID, or the content is refused: a blank or long \
                                      label, a malformed or repeated time slot, a window that is \
                                      not whole, or content over 1 MiB. The detail names the field", body = crate::openapi_admin::ErrorResponse),
        (status = 401, description = "No session", body = crate::openapi_admin::ErrorResponse),
        (status = 403, description = "The role lacks console.packages.manage", body = crate::openapi_admin::ErrorResponse),
        (status = 404, description = "The tenant has no package with that id", body = crate::openapi_admin::ErrorResponse),
        (status = 409, description = "A version already holds that id", body = crate::openapi_admin::ErrorResponse),
        (status = 503, description = "The package store is unreachable, or the OS would not give the entropy the id needs", body = crate::openapi_admin::ErrorResponse),
    ),
    tag = "packages",
)]
/// A console user publishes a version of a package. It never changes afterwards.
pub(super) async fn admin_publish_package_version<P, A, C>(
    State(state): State<PackageState<P, A, C>>,
    headers: HeaderMap,
    Path(package_id): Path<String>,
    Json(request): Json<PackageVersionRequest>,
) -> Response
where
    P: PackageStore + Clone + Send + Sync + 'static,
    A: AdminStore + Clone + Send + Sync + 'static,
    C: ClockSource + Clone + Send + Sync + 'static,
{
    let actor = match require_permission(
        &state.admin,
        &state.clock,
        &headers,
        ConsolePermission::ManagePackages,
    )
    .await
    {
        Ok(actor) => actor,
        Err(denied) => return denied,
    };
    let (tenant_id, package_id) = match parse_ulid_fields([
        ("tenant_id", &request.tenant_id),
        ("package_id", &package_id),
    ]) {
        Ok([tenant_id, package_id]) => (TenantId::new(tenant_id), PackageId::new(package_id)),
        Err(refusal) => return refusal,
    };
    match state.packages.get_package(tenant_id, package_id).await {
        Ok(Some(_)) => {}
        Ok(None) => return not_found("package"),
        Err(error) => return package_error_response(&error),
    }
    let now = state.clock.now();
    let Some(package_version_id) =
        mint_ulid(now.as_milliseconds_since_epoch()).map(PackageVersionId::new)
    else {
        return package_entropy_unavailable();
    };
    let mut content = PackageContent {
        package_id,
        package_version_id,
        version_label: request.version_label.trim().to_owned(),
        min_edge_version: ReleaseTag::new(""),
        time_slots: request.time_slots,
    };
    content.min_edge_version = min_edge_version(&content);
    if let Err(refusal) = check_content(&content) {
        return api_error_with_details(
            ErrorStatus::InvalidArgument,
            refusal.to_string(),
            &[(refusal.field(), "INVALID")],
        );
    }
    if serde_json::to_vec(&content).map_or(true, |bytes| bytes.len() > CONTENT_BYTES_MAX) {
        return FieldRefusal::new(
            "a version's content is at most 1 MiB",
            "content",
            "TOO_LARGE",
        )
        .into_response();
    }
    let version = PackageVersionRecord {
        content,
        create_time: now,
    };
    match state.packages.publish_version(tenant_id, &version).await {
        Ok(CreateOutcome::Created(_)) => {
            audit_action(
                &state.audit,
                &state.clock,
                &actor,
                Some(tenant_id),
                "packages.version.publish",
                "package_version",
                &package_version_id.to_string(),
                None,
                serde_json::to_value(&version.content).ok(),
            )
            .await;
            (StatusCode::CREATED, Json(version)).into_response()
        }
        Ok(CreateOutcome::AlreadyExists) => already_exists("package version"),
        Err(error) => package_error_response(&error),
    }
}
