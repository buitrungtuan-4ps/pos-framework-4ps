// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! `/admin/integrations/gateway-endpoints`: list, read, create, update and delete a tenant's gateway
//! endpoints ([ADR-0198](../../../../docs/adr/0198-a-store-reaches-the-internet-only-through-the-cloud.md)
//! decisions 2 to 5).
//!
//! Reads are behind `console.data.read` and never answer a secret header's value. Writes are behind
//! `console.integrations.manage`, vet the URL with the webhook guard before anything is stored, and
//! seal every secret header with the connections' key.

use std::collections::BTreeMap;
use std::sync::Arc;

use axum::Router;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use pos_cloud_api::console::{audit_action, mint_ulid, require_permission};
use pos_cloud_api::response::{
    api_error, api_error_with_details, parse_ulid_fields, service_unavailable,
};
use pos_proto::ErrorStatus;
use pos_proto::determinism::ClockSource;
use pos_proto::ids::TenantId;
use serde::{Deserialize, Serialize};

use crate::audit::AuditRecorder;
use crate::auth::admin::AdminStore;
use crate::auth::console_rbac::ConsolePermission;
use crate::connections::SealedFor;
use crate::gateway::endpoints::{
    EndpointRefusal, GatewayEndpoint, GatewayEndpointStore, GatewayEndpointStoreError,
    GatewayEndpointView, GatewayField, GatewayMethod, MAX_SECRET_HEADERS, check_fields,
    check_header_value, checked_deadline, checked_header_name, checked_name,
};
use crate::http::{
    FieldRefusal, Json, RegistryTenantQuery, already_exists, if_match, not_found, version_mismatch,
    versioned_created, versioned_ok,
};
use crate::keys::Keys;
use crate::version::{CreateOutcome, UpdateOutcome, Versioned};
use crate::webhook::ssrf::{resolve_host, vet};

/// What the endpoint routes share.
#[derive(Clone)]
pub(super) struct EndpointState<S, A, C> {
    endpoints: S,
    keys: Option<Keys>,
    admin: A,
    clock: C,
    audit: Arc<dyn AuditRecorder>,
}

/// A create or update body. The id is server-owned: minted on create, the path id on update.
///
/// `secret_headers` holds only the values this write sets. A header left out keeps the value the
/// endpoint already holds, because the console cannot show it to be sent back;
/// `clear_secret_headers` removes one on purpose.
#[derive(Debug, Clone, Deserialize, utoipa::ToSchema)]
pub(in crate::http) struct GatewayEndpointRequest {
    tenant_id: String,
    display_name: String,
    /// The `https` URL. Vetted by the webhook guard before it is stored, and before every call.
    url: String,
    method: GatewayMethod,
    #[serde(default)]
    request_fields: Vec<GatewayField>,
    #[serde(default)]
    response_fields: Vec<GatewayField>,
    /// In milliseconds; absent is 3000, and at most 10000.
    #[serde(default)]
    deadline_ms: Option<u32>,
    /// Whether a call may be retried on a transport failure or a `5xx`.
    #[serde(default)]
    idempotent: bool,
    /// Absent means enabled.
    #[serde(default = "endpoint_is_enabled")]
    enabled: bool,
    #[serde(default)]
    secret_headers: BTreeMap<String, String>,
    #[serde(default)]
    clear_secret_headers: Vec<String>,
}

/// The default for [`GatewayEndpointRequest::enabled`].
const fn endpoint_is_enabled() -> bool {
    true
}

/// What the audit trail keeps of an endpoint: never a secret, and the URL without its query, which
/// may carry a token.
#[derive(Debug, Clone, Serialize)]
struct EndpointAuditSummary {
    gateway_endpoint_id: String,
    display_name: String,
    url: String,
    method: GatewayMethod,
    request_fields: Vec<String>,
    response_fields: Vec<String>,
    deadline_ms: u32,
    idempotent: bool,
    enabled: bool,
    secret_headers_set: Vec<String>,
}

impl EndpointAuditSummary {
    fn of(endpoint: &GatewayEndpoint) -> Self {
        let url = url::Url::parse(&endpoint.url).map_or_else(
            |_| String::new(),
            |mut parsed| {
                parsed.set_query(None);
                parsed.set_fragment(None);
                parsed.to_string()
            },
        );
        Self {
            gateway_endpoint_id: endpoint.gateway_endpoint_id.clone(),
            display_name: endpoint.display_name.clone(),
            url,
            method: endpoint.method,
            request_fields: names(&endpoint.request_fields),
            response_fields: names(&endpoint.response_fields),
            deadline_ms: endpoint.deadline_ms,
            idempotent: endpoint.idempotent,
            enabled: endpoint.enabled,
            secret_headers_set: endpoint.secret_headers_set().into_iter().collect(),
        }
    }
}

fn names(fields: &[GatewayField]) -> Vec<String> {
    fields.iter().map(|field| field.name.clone()).collect()
}

/// Builds the endpoint sub-router.
pub fn gateway_endpoint_router<S, A, C>(
    endpoints: S,
    keys: Option<Keys>,
    admin: A,
    clock: C,
    audit: Arc<dyn AuditRecorder>,
) -> Router
where
    S: GatewayEndpointStore + Clone + Send + Sync + 'static,
    A: AdminStore + Clone + Send + Sync + 'static,
    C: ClockSource + Clone + Send + Sync + 'static,
{
    Router::new()
        .route(
            "/admin/integrations/gateway-endpoints",
            get(admin_list_gateway_endpoints::<S, A, C>)
                .post(admin_create_gateway_endpoint::<S, A, C>),
        )
        .route(
            "/admin/integrations/gateway-endpoints/{gateway_endpoint_id}",
            get(admin_get_gateway_endpoint::<S, A, C>)
                .put(admin_update_gateway_endpoint::<S, A, C>)
                .delete(admin_delete_gateway_endpoint::<S, A, C>),
        )
        .with_state(EndpointState {
            endpoints,
            keys,
            admin,
            clock,
            audit,
        })
}

#[utoipa::path(
    get,
    path = "/admin/integrations/gateway-endpoints",
    security(("session_cookie" = [])),
    extensions(("x-permission" = json!("console.data.read"))),
    params(("tenant_id" = String, Query, description = "The tenant whose endpoints to read (a 26-character ULID)")),
    responses(
        (status = 200, body = [Versioned<GatewayEndpointView>], description = "The tenant's gateway endpoints in id order, each with \
                                      the version it was read at. A secret header's value is never \
                                      returned: `secret_headers_set` names the ones that have one"),
        (status = 400, description = "tenant_id is absent or not a ULID", body = crate::openapi_admin::ErrorResponse),
        (status = 401, description = "No session", body = crate::openapi_admin::ErrorResponse),
        (status = 403, description = "The role lacks console.data.read", body = crate::openapi_admin::ErrorResponse),
        (status = 503, description = "The endpoint store is unreachable", body = crate::openapi_admin::ErrorResponse),
    ),
    tag = "integrations",
)]
/// A console user lists a tenant's gateway endpoints.
pub(super) async fn admin_list_gateway_endpoints<S, A, C>(
    State(state): State<EndpointState<S, A, C>>,
    headers: HeaderMap,
    Query(query): Query<RegistryTenantQuery>,
) -> Response
where
    S: GatewayEndpointStore + Clone + Send + Sync + 'static,
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
    match state.endpoints.list(tenant_id).await {
        Ok(rows) => {
            let views: Vec<Versioned<GatewayEndpointView>> = rows
                .into_iter()
                .map(|row| Versioned {
                    record: GatewayEndpointView::of(&row.record),
                    etag: row.etag,
                    updated_at_ms: row.updated_at_ms,
                })
                .collect();
            (StatusCode::OK, Json(views)).into_response()
        }
        Err(error) => store_error(&error),
    }
}

#[utoipa::path(
    get,
    path = "/admin/integrations/gateway-endpoints/{gateway_endpoint_id}",
    security(("session_cookie" = [])),
    extensions(("x-permission" = json!("console.data.read"))),
    params(
        ("gateway_endpoint_id" = String, Path, description = "The endpoint's 26-character ULID"),
        ("tenant_id" = String, Query, description = "The tenant that owns it (a 26-character ULID)"),
    ),
    responses(
        (status = 200, body = Versioned<GatewayEndpointView>, description = "The endpoint without its secret values, with an ETag \
                                      carrying the version it was read at"),
        (status = 400, description = "tenant_id or gateway_endpoint_id is absent or not a ULID", body = crate::openapi_admin::ErrorResponse),
        (status = 401, description = "No session", body = crate::openapi_admin::ErrorResponse),
        (status = 403, description = "The role lacks console.data.read", body = crate::openapi_admin::ErrorResponse),
        (status = 404, description = "The tenant has no endpoint with that id", body = crate::openapi_admin::ErrorResponse),
        (status = 503, description = "The endpoint store is unreachable", body = crate::openapi_admin::ErrorResponse),
    ),
    tag = "integrations",
)]
/// A console user reads one gateway endpoint.
pub(super) async fn admin_get_gateway_endpoint<S, A, C>(
    State(state): State<EndpointState<S, A, C>>,
    headers: HeaderMap,
    Path(gateway_endpoint_id): Path<String>,
    Query(query): Query<RegistryTenantQuery>,
) -> Response
where
    S: GatewayEndpointStore + Clone + Send + Sync + 'static,
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
    let (tenant_id, endpoint_id) = match parse_ulid_fields([
        ("tenant_id", &query.tenant_id),
        ("gateway_endpoint_id", &gateway_endpoint_id),
    ]) {
        Ok([tenant_id, endpoint_id]) => (TenantId::new(tenant_id), endpoint_id.to_string()),
        Err(refusal) => return refusal,
    };
    match state.endpoints.get(tenant_id, &endpoint_id).await {
        Ok(Some(row)) => versioned_ok(GatewayEndpointView::of(&row.record), &row.etag),
        Ok(None) => not_found("gateway endpoint"),
        Err(error) => store_error(&error),
    }
}

#[utoipa::path(
    post,
    path = "/admin/integrations/gateway-endpoints",
    security(("session_cookie" = [])),
    extensions(("x-permission" = json!("console.integrations.manage"))),
    request_body(
        content = GatewayEndpointRequest, description = "The tenant, a display name, the `https` url, the method, the request and \
                       response fields by name and kind, `deadline_ms` (3000 when absent, at most \
                       10000), `idempotent`, `enabled`, and `secret_headers`. The id is \
                       server-minted. The URL is vetted before anything is stored; secret headers \
                       are sealed on arrival and never returned",
        content_type = "application/json",
    ),
    responses(
        (status = 201, body = Versioned<GatewayEndpointView>, description = "Created. The body is the endpoint without its secret \
                                      values, including the id the server minted"),
        (status = 400, description = "A field is wrong, named in `details`: the name, the URL (not \
                                      https, carrying credentials, or not resolving only to public \
                                      addresses), a field name, the deadline, or a header", body = crate::openapi_admin::ErrorResponse),
        (status = 401, description = "No session", body = crate::openapi_admin::ErrorResponse),
        (status = 403, description = "The role lacks console.integrations.manage", body = crate::openapi_admin::ErrorResponse),
        (status = 503, description = "The endpoint store or the URL check is unavailable, or a \
                                      secret header was sent and this cloud has no \
                                      integration_secret to seal it with", body = crate::openapi_admin::ErrorResponse),
    ),
    tag = "integrations",
)]
/// A console user declares a gateway endpoint; the server mints the id.
pub(super) async fn admin_create_gateway_endpoint<S, A, C>(
    State(state): State<EndpointState<S, A, C>>,
    headers: HeaderMap,
    Json(request): Json<GatewayEndpointRequest>,
) -> Response
where
    S: GatewayEndpointStore + Clone + Send + Sync + 'static,
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
    let tenant_id = match parse_ulid_fields([("tenant_id", &request.tenant_id)]) {
        Ok([tenant_id]) => TenantId::new(tenant_id),
        Err(refusal) => return refusal,
    };
    let Some(endpoint_id) = mint_ulid(state.clock.now().as_milliseconds_since_epoch()) else {
        return service_unavailable("id");
    };
    let endpoint_id = endpoint_id.to_string();
    let endpoint = match build_endpoint(&state, &request, tenant_id, &endpoint_id, None).await {
        Ok(endpoint) => endpoint,
        Err(refusal) => return refusal,
    };
    match state.endpoints.create(tenant_id, &endpoint).await {
        Ok(CreateOutcome::Created(version)) => {
            audit_action(
                &state.audit,
                &state.clock,
                &context,
                Some(tenant_id),
                "integrations.gateway_endpoint.create",
                "gateway_endpoint",
                &endpoint_id,
                None,
                serde_json::to_value(EndpointAuditSummary::of(&endpoint)).ok(),
            )
            .await;
            versioned_created(GatewayEndpointView::of(&endpoint), &version)
        }
        Ok(CreateOutcome::AlreadyExists) => already_exists("gateway endpoint"),
        Err(error) => store_error(&error),
    }
}

#[utoipa::path(
    put,
    path = "/admin/integrations/gateway-endpoints/{gateway_endpoint_id}",
    security(("session_cookie" = [])),
    extensions(("x-permission" = json!("console.integrations.manage"))),
    params(
        ("gateway_endpoint_id" = String, Path, description = "The endpoint's 26-character ULID"),
        ("If-Match" = String, Header, description = "The ETag the endpoint was read at \
                                                     (ADR-0094). Required; a wildcard is refused"),
    ),
    request_body(
        content = GatewayEndpointRequest, description = "The endpoint as it should now stand. A secret header left out of \
                       `secret_headers` keeps its stored value; `clear_secret_headers` removes one",
        content_type = "application/json",
    ),
    responses(
        (status = 200, body = Versioned<GatewayEndpointView>, description = "Updated. The body is the endpoint without its secret \
                                      values, and the ETag carries its new version"),
        (status = 400, description = "A field is wrong, named in `details`, or If-Match is \
                                      absent, malformed, or a wildcard", body = crate::openapi_admin::ErrorResponse),
        (status = 401, description = "No session", body = crate::openapi_admin::ErrorResponse),
        (status = 403, description = "The role lacks console.integrations.manage", body = crate::openapi_admin::ErrorResponse),
        (status = 404, description = "The tenant has no endpoint with that id", body = crate::openapi_admin::ErrorResponse),
        (status = 412, description = "The endpoint changed since you read it: re-read it and try \
                                      again", body = crate::openapi_admin::ErrorResponse),
        (status = 503, description = "The endpoint store or the URL check is unavailable, or a \
                                      secret header cannot be sealed", body = crate::openapi_admin::ErrorResponse),
    ),
    tag = "integrations",
)]
/// A console user edits a gateway endpoint in place.
pub(super) async fn admin_update_gateway_endpoint<S, A, C>(
    State(state): State<EndpointState<S, A, C>>,
    headers: HeaderMap,
    Path(gateway_endpoint_id): Path<String>,
    Json(request): Json<GatewayEndpointRequest>,
) -> Response
where
    S: GatewayEndpointStore + Clone + Send + Sync + 'static,
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
    let (tenant_id, endpoint_id) = match parse_ulid_fields([
        ("tenant_id", &request.tenant_id),
        ("gateway_endpoint_id", &gateway_endpoint_id),
    ]) {
        Ok([tenant_id, endpoint_id]) => (TenantId::new(tenant_id), endpoint_id.to_string()),
        Err(refusal) => return refusal,
    };
    let expected = match if_match(&headers) {
        Ok(expected) => expected,
        Err(refusal) => return refusal,
    };
    let before = match state.endpoints.get(tenant_id, &endpoint_id).await {
        Ok(Some(row)) => row.record,
        Ok(None) => return not_found("gateway endpoint"),
        Err(error) => return store_error(&error),
    };
    let endpoint =
        match build_endpoint(&state, &request, tenant_id, &endpoint_id, Some(&before)).await {
            Ok(endpoint) => endpoint,
            Err(refusal) => return refusal,
        };
    match state
        .endpoints
        .update(tenant_id, &endpoint, &expected)
        .await
    {
        Ok(UpdateOutcome::Updated(version)) => {
            audit_action(
                &state.audit,
                &state.clock,
                &context,
                Some(tenant_id),
                "integrations.gateway_endpoint.update",
                "gateway_endpoint",
                &endpoint_id,
                serde_json::to_value(EndpointAuditSummary::of(&before)).ok(),
                serde_json::to_value(EndpointAuditSummary::of(&endpoint)).ok(),
            )
            .await;
            versioned_ok(GatewayEndpointView::of(&endpoint), &version)
        }
        Ok(UpdateOutcome::VersionMismatch) => version_mismatch(),
        Ok(UpdateOutcome::NotFound) => not_found("gateway endpoint"),
        Err(error) => store_error(&error),
    }
}

#[utoipa::path(
    delete,
    path = "/admin/integrations/gateway-endpoints/{gateway_endpoint_id}",
    security(("session_cookie" = [])),
    extensions(("x-permission" = json!("console.integrations.manage"))),
    params(
        ("gateway_endpoint_id" = String, Path, description = "The endpoint's 26-character ULID"),
        ("tenant_id" = String, Query, description = "The tenant that owns it (a 26-character ULID)"),
    ),
    responses(
        (status = 204, description = "Deleted, with its sealed headers"),
        (status = 400, description = "tenant_id or gateway_endpoint_id is absent or not a ULID", body = crate::openapi_admin::ErrorResponse),
        (status = 401, description = "No session", body = crate::openapi_admin::ErrorResponse),
        (status = 403, description = "The role lacks console.integrations.manage", body = crate::openapi_admin::ErrorResponse),
        (status = 404, description = "The tenant has no endpoint with that id", body = crate::openapi_admin::ErrorResponse),
        (status = 503, description = "The endpoint store is unreachable", body = crate::openapi_admin::ErrorResponse),
    ),
    tag = "integrations",
)]
/// A console user removes a gateway endpoint and the credentials it held.
pub(super) async fn admin_delete_gateway_endpoint<S, A, C>(
    State(state): State<EndpointState<S, A, C>>,
    headers: HeaderMap,
    Path(gateway_endpoint_id): Path<String>,
    Query(query): Query<RegistryTenantQuery>,
) -> Response
where
    S: GatewayEndpointStore + Clone + Send + Sync + 'static,
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
    let (tenant_id, endpoint_id) = match parse_ulid_fields([
        ("tenant_id", &query.tenant_id),
        ("gateway_endpoint_id", &gateway_endpoint_id),
    ]) {
        Ok([tenant_id, endpoint_id]) => (TenantId::new(tenant_id), endpoint_id.to_string()),
        Err(refusal) => return refusal,
    };
    let before = match state.endpoints.get(tenant_id, &endpoint_id).await {
        Ok(Some(row)) => row.record,
        Ok(None) => return not_found("gateway endpoint"),
        Err(error) => return store_error(&error),
    };
    match state.endpoints.delete(tenant_id, &endpoint_id).await {
        Ok(()) => {
            audit_action(
                &state.audit,
                &state.clock,
                &context,
                Some(tenant_id),
                "integrations.gateway_endpoint.delete",
                "gateway_endpoint",
                &endpoint_id,
                serde_json::to_value(EndpointAuditSummary::of(&before)).ok(),
                None,
            )
            .await;
            StatusCode::NO_CONTENT.into_response()
        }
        Err(error) => store_error(&error),
    }
}

/// The `400` an [`EndpointRefusal`] becomes.
fn refused(refusal: EndpointRefusal) -> Response {
    FieldRefusal::new(refusal.message, refusal.field, refusal.reason).into_response()
}

/// Checks a request and builds the record to store: the URL vetted, the fields checked, and every
/// secret header this write sets sealed. `existing` is the endpoint as stored, on an update: its
/// sealed headers are kept unless the request replaces or clears them.
async fn build_endpoint<S, A, C>(
    state: &EndpointState<S, A, C>,
    request: &GatewayEndpointRequest,
    tenant_id: TenantId,
    endpoint_id: &str,
    existing: Option<&GatewayEndpoint>,
) -> Result<GatewayEndpoint, Response> {
    let display_name = checked_name(&request.display_name).map_err(refused)?;
    check_fields(&request.request_fields, "request_fields").map_err(refused)?;
    check_fields(&request.response_fields, "response_fields").map_err(refused)?;
    let deadline_ms = checked_deadline(request.deadline_ms).map_err(refused)?;

    let mut sealed = existing
        .map(|existing| existing.sealed_headers.clone())
        .unwrap_or_default();
    for name in &request.clear_secret_headers {
        sealed.remove(&checked_header_name(name).map_err(refused)?);
    }
    let mut setting = BTreeMap::new();
    for (name, value) in &request.secret_headers {
        check_header_value(value).map_err(refused)?;
        setting.insert(checked_header_name(name).map_err(refused)?, value.as_str());
    }
    if sealed
        .keys()
        .chain(setting.keys())
        .collect::<std::collections::BTreeSet<_>>()
        .len()
        > MAX_SECRET_HEADERS
    {
        return Err(refused(EndpointRefusal {
            field: "secret_headers",
            reason: "TOO_MANY",
            message: "an endpoint holds at most 8 secret headers",
        }));
    }

    // The URL last among the checks: it may resolve a name, and a malformed body should not cost a
    // DNS lookup. The submitted URL is never logged: it may carry a token in its query.
    let raw_url = request.url.clone();
    let url = match tokio::task::spawn_blocking(move || vet(&raw_url, resolve_host)).await {
        Ok(Ok(vetted)) => vetted.url,
        Ok(Err(rejection)) => {
            tracing::warn!(reason = %rejection, "refused a gateway endpoint URL");
            return Err(api_error_with_details(
                ErrorStatus::InvalidArgument,
                "the endpoint URL is refused: it must be https, carry no credentials, and \
                 resolve only to public addresses",
                &[("url", rejection.caller_reason())],
            ));
        }
        Err(join_error) => {
            tracing::error!(%join_error, "the SSRF vetting task failed to join");
            return Err(service_unavailable("gateway endpoint"));
        }
    };

    if !setting.is_empty() {
        let Some(keys) = state.keys.as_ref() else {
            return Err(api_error(
                ErrorStatus::Unavailable,
                "this cloud has no integration_secret in cloud.toml, so it cannot store a \
                 credential: add one (openssl rand -hex 32) and restart",
            ));
        };
        let Ok(tenant_keys) = keys.for_tenant(tenant_id).await else {
            return Err(service_unavailable("tenant keys"));
        };
        for (name, value) in setting {
            let field_key = format!("header:{name}");
            let sealed_for = SealedFor {
                tenant_id,
                connection_id: endpoint_id,
                field_key: &field_key,
            };
            match tenant_keys.seal(sealed_for, value) {
                Ok(value) => {
                    sealed.insert(name, value);
                }
                Err(error) => {
                    tracing::error!(%error, "a gateway endpoint header could not be sealed");
                    return Err(service_unavailable("integration secret"));
                }
            }
        }
    }

    Ok(GatewayEndpoint {
        gateway_endpoint_id: endpoint_id.to_owned(),
        display_name,
        url,
        method: request.method,
        request_fields: request.request_fields.clone(),
        response_fields: request.response_fields.clone(),
        deadline_ms,
        idempotent: request.idempotent,
        enabled: request.enabled,
        sealed_headers: sealed,
    })
}

/// The `503` an endpoint-store failure becomes. The error goes to the log; it never carries a
/// secret value, because the store only holds sealed ones.
fn store_error(error: &GatewayEndpointStoreError) -> Response {
    tracing::error!(%error, "a gateway endpoint store operation failed");
    api_error(
        ErrorStatus::Unavailable,
        "the gateway endpoint service is unavailable",
    )
}
