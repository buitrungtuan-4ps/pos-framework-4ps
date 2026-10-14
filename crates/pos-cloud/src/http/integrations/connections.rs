// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The `/admin/integrations/connections` handlers: list, read, create, update and delete.

use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use pos_cloud_api::console::{audit_action, mint_ulid, require_permission};
use pos_cloud_api::response::{api_error, parse_ulid_fields, service_unavailable};
use pos_proto::ErrorStatus;
use pos_proto::determinism::ClockSource;
use pos_proto::ids::TenantId;

use super::{
    ConnectionAuditSummary, ConnectionRequest, ConnectionState, build_connection,
    exclusive_scope_taken,
};
use crate::auth::admin::AdminStore;
use crate::auth::console_rbac::ConsolePermission;
use crate::http::{
    Json, RegistryTenantQuery, already_exists, if_match, not_found, version_mismatch,
    versioned_created, versioned_ok,
};
use crate::version::{CreateOutcome, UpdateOutcome, Versioned};

#[utoipa::path(
    get,
    path = "/admin/integrations/connections",
    params(("tenant_id" = String, Query, description = "The tenant whose connections to read (a 26-character ULID)")),
    responses(
        (status = 200, description = "The tenant's connections in id order, each with the version \
                                      it was read at. A secret field's value is never returned: \
                                      `secrets_set` names the ones that have one"),
        (status = 400, description = "tenant_id is absent or not a ULID", body = crate::openapi_admin::ErrorResponse),
        (status = 401, description = "No session", body = crate::openapi_admin::ErrorResponse),
        (status = 403, description = "The role lacks console.read", body = crate::openapi_admin::ErrorResponse),
        (status = 503, description = "The connection store is unreachable", body = crate::openapi_admin::ErrorResponse),
    ),
    tag = "integrations",
)]
/// A console user lists a tenant's vendor connections.
pub(super) async fn admin_list_connections<S, A, C>(
    State(state): State<ConnectionState<S, A, C>>,
    headers: HeaderMap,
    Query(query): Query<RegistryTenantQuery>,
) -> Response
where
    S: crate::connections::ConnectionStore + Clone + Send + Sync + 'static,
    A: AdminStore + Clone + Send + Sync + 'static,
    C: ClockSource + Clone + Send + Sync + 'static,
{
    use crate::connections::ConnectionView;

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
    match state.connections.list(tenant_id).await {
        Ok(rows) => {
            let views: Vec<Versioned<ConnectionView>> = rows
                .into_iter()
                .map(|row| Versioned {
                    record: ConnectionView::of(&row.record),
                    etag: row.etag,
                    updated_at_ms: row.updated_at_ms,
                })
                .collect();
            (StatusCode::OK, Json(views)).into_response()
        }
        Err(error) => connection_error_response(&error),
    }
}

#[utoipa::path(
    get,
    path = "/admin/integrations/connections/{connection_id}",
    params(
        ("connection_id" = String, Path, description = "The connection's 26-character ULID"),
        ("tenant_id" = String, Query, description = "The tenant that owns it (a 26-character ULID)"),
    ),
    responses(
        (status = 200, description = "The connection without its secret values, with an ETag \
                                      carrying the version it was read at"),
        (status = 400, description = "tenant_id or connection_id is absent or not a ULID", body = crate::openapi_admin::ErrorResponse),
        (status = 401, description = "No session", body = crate::openapi_admin::ErrorResponse),
        (status = 403, description = "The role lacks console.read", body = crate::openapi_admin::ErrorResponse),
        (status = 404, description = "The tenant has no connection with that id", body = crate::openapi_admin::ErrorResponse),
        (status = 503, description = "The connection store is unreachable", body = crate::openapi_admin::ErrorResponse),
    ),
    tag = "integrations",
)]
/// A console user reads one vendor connection.
pub(super) async fn admin_get_connection<S, A, C>(
    State(state): State<ConnectionState<S, A, C>>,
    headers: HeaderMap,
    Path(connection_id): Path<String>,
    Query(query): Query<RegistryTenantQuery>,
) -> Response
where
    S: crate::connections::ConnectionStore + Clone + Send + Sync + 'static,
    A: AdminStore + Clone + Send + Sync + 'static,
    C: ClockSource + Clone + Send + Sync + 'static,
{
    use crate::connections::ConnectionView;

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
    let (tenant_id, connection_id) = match parse_ulid_fields([
        ("tenant_id", &query.tenant_id),
        ("connection_id", &connection_id),
    ]) {
        Ok([tenant_id, connection_id]) => (TenantId::new(tenant_id), connection_id.to_string()),
        Err(refusal) => return refusal,
    };
    match state.connections.get(tenant_id, &connection_id).await {
        Ok(Some(row)) => versioned_ok(ConnectionView::of(&row.record), &row.etag),
        Ok(None) => not_found("connection"),
        Err(error) => connection_error_response(&error),
    }
}

#[utoipa::path(
    post,
    path = "/admin/integrations/connections",
    request_body(
        description = "The tenant, the provider id, the scope (`scope_level` and, for a brand or \
                       a store, `scope_id`), a display name, `enabled`, the provider's `settings`, \
                       and its `secrets`. The id is server-minted. Secrets are sealed on arrival \
                       and never returned",
        content_type = "application/json",
    ),
    responses(
        (status = 201, description = "Created. The body is the connection without its secret \
                                      values, including the id the server minted"),
        (status = 400, description = "The name is blank or too long, the provider is unknown, the \
                                      scope is malformed, or a setting does not match the \
                                      provider's schema — each named in `details`", body = crate::openapi_admin::ErrorResponse),
        (status = 401, description = "No session", body = crate::openapi_admin::ErrorResponse),
        (status = 403, description = "The role lacks console.integrations.manage", body = crate::openapi_admin::ErrorResponse),
        (status = 409, description = "The scope already has an enabled connection in a family it \
                                      may have only one of (e-invoice, ERP)", body = crate::openapi_admin::ErrorResponse),
        (status = 503, description = "The connection store is unreachable, or a secret was sent \
                                      and this cloud has no integration_secret to seal it with", body = crate::openapi_admin::ErrorResponse),
    ),
    tag = "integrations",
)]
/// A console user connects a tenant, brand or store to a vendor; the server mints the id.
pub(super) async fn admin_create_connection<S, A, C>(
    State(state): State<ConnectionState<S, A, C>>,
    headers: HeaderMap,
    Json(request): Json<ConnectionRequest>,
) -> Response
where
    S: crate::connections::ConnectionStore + Clone + Send + Sync + 'static,
    A: AdminStore + Clone + Send + Sync + 'static,
    C: ClockSource + Clone + Send + Sync + 'static,
{
    use crate::connections::ConnectionView;

    let context = match require_permission(
        &state.admin,
        &state.clock,
        &headers,
        ConsolePermission::ManageIntegrations,
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
    let Some(connection_id) = mint_ulid(state.clock.now().as_milliseconds_since_epoch()) else {
        return service_unavailable("id");
    };
    let connection_id = connection_id.to_string();
    let connection = match build_connection(
        &request,
        tenant_id,
        &connection_id,
        &state.providers,
        state.integration_secret.as_ref(),
        None,
    ) {
        Ok(connection) => connection,
        Err(refusal) => return refusal,
    };
    match exclusive_scope_taken(&state.connections, &state.providers, tenant_id, &connection).await
    {
        Ok(Some(refusal)) | Err(refusal) => return refusal,
        Ok(None) => {}
    }
    match state.connections.create(tenant_id, &connection).await {
        Ok(CreateOutcome::Created(version)) => {
            audit_action(
                &state.audit,
                &state.clock,
                &context,
                Some(tenant_id),
                "integrations.connection.create",
                "connection",
                &connection_id,
                None,
                serde_json::to_value(ConnectionAuditSummary::of(&connection)).ok(),
            )
            .await;
            versioned_created(ConnectionView::of(&connection), &version)
        }
        Ok(CreateOutcome::AlreadyExists) => already_exists("connection"),
        Err(error) => connection_error_response(&error),
    }
}

#[utoipa::path(
    put,
    path = "/admin/integrations/connections/{connection_id}",
    params(
        ("connection_id" = String, Path, description = "The connection's 26-character ULID"),
        ("If-Match" = String, Header, description = "The ETag the connection was read at \
                                                     (ADR-0094). Required; a wildcard is refused"),
    ),
    request_body(
        description = "The connection as it should now stand. A secret left out of `secrets` keeps \
                       its stored value; `clear_secrets` removes one; a changed `provider_id` drops \
                       them all. Switching vendor is this route",
        content_type = "application/json",
    ),
    responses(
        (status = 200, description = "Updated. The body is the connection without its secret \
                                      values, and the ETag carries its new version"),
        (status = 400, description = "A field is wrong, each named in `details`, or If-Match is \
                                      absent, malformed, or a wildcard", body = crate::openapi_admin::ErrorResponse),
        (status = 401, description = "No session", body = crate::openapi_admin::ErrorResponse),
        (status = 403, description = "The role lacks console.integrations.manage", body = crate::openapi_admin::ErrorResponse),
        (status = 404, description = "The tenant has no connection with that id", body = crate::openapi_admin::ErrorResponse),
        (status = 409, description = "Enabling it would give the scope a second connection in a \
                                      family it may have only one of", body = crate::openapi_admin::ErrorResponse),
        (status = 412, description = "The connection changed since you read it: re-read it and \
                                      try again", body = crate::openapi_admin::ErrorResponse),
        (status = 503, description = "The connection store is unreachable, or a secret was sent \
                                      and this cloud cannot seal it", body = crate::openapi_admin::ErrorResponse),
    ),
    tag = "integrations",
)]
/// A console user edits a vendor connection in place — its settings, its credentials, whether it is
/// enabled, or which vendor it uses.
pub(super) async fn admin_update_connection<S, A, C>(
    State(state): State<ConnectionState<S, A, C>>,
    headers: HeaderMap,
    Path(connection_id): Path<String>,
    Json(request): Json<ConnectionRequest>,
) -> Response
where
    S: crate::connections::ConnectionStore + Clone + Send + Sync + 'static,
    A: AdminStore + Clone + Send + Sync + 'static,
    C: ClockSource + Clone + Send + Sync + 'static,
{
    use crate::connections::ConnectionView;

    let context = match require_permission(
        &state.admin,
        &state.clock,
        &headers,
        ConsolePermission::ManageIntegrations,
    )
    .await
    {
        Ok(context) => context,
        Err(denied) => return denied,
    };
    let (tenant_id, connection_id) = match parse_ulid_fields([
        ("tenant_id", &request.tenant_id),
        ("connection_id", &connection_id),
    ]) {
        Ok([tenant_id, connection_id]) => (TenantId::new(tenant_id), connection_id.to_string()),
        Err(refusal) => return refusal,
    };
    let expected = match if_match(&headers) {
        Ok(expected) => expected,
        Err(refusal) => return refusal,
    };
    let before = match state.connections.get(tenant_id, &connection_id).await {
        Ok(Some(row)) => row.record,
        Ok(None) => return not_found("connection"),
        Err(error) => return connection_error_response(&error),
    };
    let connection = match build_connection(
        &request,
        tenant_id,
        &connection_id,
        &state.providers,
        state.integration_secret.as_ref(),
        Some(&before),
    ) {
        Ok(connection) => connection,
        Err(refusal) => return refusal,
    };
    match exclusive_scope_taken(&state.connections, &state.providers, tenant_id, &connection).await
    {
        Ok(Some(refusal)) | Err(refusal) => return refusal,
        Ok(None) => {}
    }
    match state
        .connections
        .update(tenant_id, &connection, &expected)
        .await
    {
        Ok(UpdateOutcome::Updated(version)) => {
            audit_action(
                &state.audit,
                &state.clock,
                &context,
                Some(tenant_id),
                "integrations.connection.update",
                "connection",
                &connection_id,
                serde_json::to_value(ConnectionAuditSummary::of(&before)).ok(),
                serde_json::to_value(ConnectionAuditSummary::of(&connection)).ok(),
            )
            .await;
            versioned_ok(ConnectionView::of(&connection), &version)
        }
        Ok(UpdateOutcome::VersionMismatch) => version_mismatch(),
        Ok(UpdateOutcome::NotFound) => not_found("connection"),
        Err(error) => connection_error_response(&error),
    }
}

#[utoipa::path(
    delete,
    path = "/admin/integrations/connections/{connection_id}",
    params(
        ("connection_id" = String, Path, description = "The connection's 26-character ULID"),
        ("tenant_id" = String, Query, description = "The tenant that owns it (a 26-character ULID)"),
    ),
    responses(
        (status = 204, description = "Deleted, with its sealed secrets"),
        (status = 400, description = "tenant_id or connection_id is absent or not a ULID", body = crate::openapi_admin::ErrorResponse),
        (status = 401, description = "No session", body = crate::openapi_admin::ErrorResponse),
        (status = 403, description = "The role lacks console.integrations.manage", body = crate::openapi_admin::ErrorResponse),
        (status = 404, description = "The tenant has no connection with that id", body = crate::openapi_admin::ErrorResponse),
        (status = 503, description = "The connection store is unreachable", body = crate::openapi_admin::ErrorResponse),
    ),
    tag = "integrations",
)]
/// A console user removes a vendor connection and the credentials it held.
pub(super) async fn admin_delete_connection<S, A, C>(
    State(state): State<ConnectionState<S, A, C>>,
    headers: HeaderMap,
    Path(connection_id): Path<String>,
    Query(query): Query<RegistryTenantQuery>,
) -> Response
where
    S: crate::connections::ConnectionStore + Clone + Send + Sync + 'static,
    A: AdminStore + Clone + Send + Sync + 'static,
    C: ClockSource + Clone + Send + Sync + 'static,
{
    let context = match require_permission(
        &state.admin,
        &state.clock,
        &headers,
        ConsolePermission::ManageIntegrations,
    )
    .await
    {
        Ok(context) => context,
        Err(denied) => return denied,
    };
    let (tenant_id, connection_id) = match parse_ulid_fields([
        ("tenant_id", &query.tenant_id),
        ("connection_id", &connection_id),
    ]) {
        Ok([tenant_id, connection_id]) => (TenantId::new(tenant_id), connection_id.to_string()),
        Err(refusal) => return refusal,
    };
    let before = match state.connections.get(tenant_id, &connection_id).await {
        Ok(Some(row)) => row.record,
        Ok(None) => return not_found("connection"),
        Err(error) => return connection_error_response(&error),
    };
    match state.connections.delete(tenant_id, &connection_id).await {
        Ok(()) => {
            audit_action(
                &state.audit,
                &state.clock,
                &context,
                Some(tenant_id),
                "integrations.connection.delete",
                "connection",
                &connection_id,
                serde_json::to_value(ConnectionAuditSummary::of(&before)).ok(),
                None,
            )
            .await;
            StatusCode::NO_CONTENT.into_response()
        }
        Err(error) => connection_error_response(&error),
    }
}

/// The `503` a connection-store failure becomes. The error goes to the log, never a secret value:
/// the store only ever holds sealed ones.
pub(super) fn connection_error_response(
    error: &crate::connections::ConnectionStoreError,
) -> Response {
    tracing::error!(%error, "a connection store operation failed");
    api_error(
        ErrorStatus::Unavailable,
        "the connection service is unavailable",
    )
}
