// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! `/admin/webhooks`: register, list, delete and re-enable a tenant's webhook endpoints.

use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use pos_cloud_api::console::{mint_ulid, require_permission};
use pos_cloud_api::response::{api_error_with_details, parse_ulid_fields, service_unavailable};
use pos_proto::ErrorStatus;
use pos_proto::determinism::ClockSource;
use pos_proto::ids::{StoreId, TenantId};
use serde::Deserialize;

use crate::auth::admin::AdminStore;
use crate::auth::console_rbac::ConsolePermission;
use crate::http::{CloudApp, Json, not_found, random_hex_32};
use crate::webhook::{
    PersistedWebhook, SigningSecret, WebhookEndpointId, WebhookEndpointStore, WebhookSummary, vet,
};

// --- Webhook endpoint administration (`/admin/webhooks`) ----------------------------------------

/// A super-admin request to register a webhook endpoint for a tenant.
#[derive(Debug, Clone, Deserialize)]
pub(in crate::http) struct RegisterWebhookRequest {
    /// The tenant the endpoint belongs to (a 26-character ULID).
    tenant_id: String,
    /// The store whose event log the endpoint follows (a 26-character ULID).
    store_id: String,
    /// The destination URL. Must be `https`, must carry no credentials, and must resolve only to
    /// public-unicast addresses — vetted here before the endpoint is stored ([ADR-0032](../../../docs/adr/0032-webhooks.md)).
    url: String,
}

/// The one-time response to a registration: the id, the normalized URL, and the signing secret shown
/// **exactly once** — the tenant needs it to verify the HMAC signature on every delivery, and the
/// cloud keeps only its own copy thereafter.
#[derive(Debug, Clone, serde::Serialize)]
struct RegisterWebhookResponse {
    /// The endpoint's public id (a ULID).
    id: String,
    /// The normalized destination URL that was stored.
    url: String,
    /// The HMAC signing secret. Shown once; it is the tenant's copy of what the cloud signs with.
    signing_secret: String,
}

/// The tenant a webhook listing or deletion is scoped to. The super-admin is global, so it names the
/// tenant on the query string, exactly as API-key provisioning and config authoring do.
#[derive(Debug, Clone, Deserialize)]
pub(in crate::http) struct WebhookTenantQuery {
    /// The tenant whose endpoints to list or delete within (a 26-character ULID).
    tenant_id: String,
}

#[utoipa::path(
    post,
    path = "/admin/webhooks",
    extensions(("x-permission" = json!("console.webhooks.manage"))),
    security(("session_cookie" = [])),
    request_body(
        description = "`tenant_id`, `store_id`, the store whose events the endpoint is sent, and \
                       `url`: `https`, carrying no credentials, and resolving only to public \
                       addresses (ADR-0032). The URL is checked before anything is stored",
        content_type = "application/json",
    ),
    responses(
        (status = 201, description = "Registered. The body carries the endpoint's `id`, the \
                                      normalised `url` that was stored, and the `signing_secret` \
                                      every delivery is signed with. The secret is returned exactly \
                                      once"),
        (status = 400, description = "tenant_id or store_id is not a ULID, or the URL is refused. \
                                      The refusal names the `url` field and does not say what this \
                                      server's resolver found", body = crate::openapi_admin::ErrorResponse),
        (status = 401, description = "No session", body = crate::openapi_admin::ErrorResponse),
        (status = 403, description = "The role lacks console.webhooks.manage", body = crate::openapi_admin::ErrorResponse),
        (status = 503, description = "The endpoint store, the URL check or the entropy source is \
                                      unavailable", body = crate::openapi_admin::ErrorResponse),
    ),
    tag = "integrator access",
)]
/// Registers a webhook endpoint and returns the one-time signing secret (super-admin only).
///
/// Behind the session guard. The destination is SSRF-vetted before anything is stored: `https` only,
/// no credentials, and every resolved address must be public unicast — so a webhook URL can never
/// become a probe into the cloud's own network ([ADR-0032](../../../docs/adr/0032-webhooks.md)). The
/// id and signing secret are minted at the edge; the secret in the `201` body is the only time it is
/// visible, because the cloud stores it to *sign* with rather than to verify a hash of.
pub(in crate::http) async fn admin_register_webhook<S, R, K, C, A, T, W>(
    State(app): State<CloudApp<S, R, K, C, A, T, W>>,
    headers: HeaderMap,
    Json(request): Json<RegisterWebhookRequest>,
) -> Response
where
    S: Clone + Send + Sync + 'static,
    R: Clone + Send + Sync + 'static,
    K: Clone + Send + Sync + 'static,
    C: ClockSource + Clone + Send + Sync + 'static,
    A: AdminStore + Clone + Send + Sync + 'static,
    T: Clone + Send + Sync + 'static,
    W: WebhookEndpointStore + Clone + Send + Sync + 'static,
{
    if let Err(denied) = require_permission(
        &app.admin,
        &app.clock,
        &headers,
        ConsolePermission::ManageWebhooks,
    )
    .await
    {
        return denied;
    }
    let (tenant_id, store_id) = match parse_ulid_fields([
        ("tenant_id", &request.tenant_id),
        ("store_id", &request.store_id),
    ]) {
        Ok([tenant_id, store_id]) => (TenantId::new(tenant_id), StoreId::new(store_id)),
        Err(refusal) => return refusal,
    };

    // Vet the destination before it is ever stored. `vet` resolves the host with a real
    // getaddrinfo-backed resolver, which blocks, so it runs on the blocking pool. The vetted
    // addresses are not kept — the delivery slice re-vets before each connect (ADR-0032) — but a
    // structurally unsafe or inward-pointing URL is refused up front rather than at first delivery.
    let raw_url = request.url.clone();
    let vetted = match tokio::task::spawn_blocking(move || {
        vet(&raw_url, crate::webhook::ssrf::resolve_host)
    })
    .await
    {
        Ok(Ok(vetted)) => vetted,
        Ok(Err(rejection)) => {
            // The full rejection goes to the log and a coarser sentence to the caller, because two
            // of the six variants are decided by *this server's* resolver rather than by the URL
            // the caller sent: rendering them told a caller which internal names resolve and to
            // what address, one name per request, which is the reconnaissance the SSRF block exists
            // to refuse. `SsrfRejection::caller_message` draws that line; the delivery path already
            // logs the same way (`webhook/runner.rs`).
            //
            // The submitted URL is deliberately absent from the log line: a webhook URL routinely
            // carries its own bearer token in the path or query, and this is a log.
            tracing::warn!(reason = %rejection, "refused to register a webhook URL");
            return api_error_with_details(
                ErrorStatus::InvalidArgument,
                rejection.caller_message(),
                &[("url", rejection.caller_reason())],
            );
        }
        Err(join_error) => {
            tracing::error!(%join_error, "the SSRF vetting task failed to join");
            return service_unavailable("webhook");
        }
    };

    let now_ms = app.clock.now().as_milliseconds_since_epoch();
    let (Some(id), Some(secret)) = (mint_webhook_id(now_ms), random_hex_32()) else {
        tracing::error!("could not read OS entropy to mint a webhook endpoint");
        return service_unavailable("webhook");
    };
    let endpoint = PersistedWebhook {
        id,
        tenant_id,
        store_id,
        url: vetted.url.clone(),
        secret: SigningSecret::new(secret.clone()),
        cursor: None,
        disabled: false,
    };
    if let Err(error) = app.webhooks.insert(&endpoint).await {
        tracing::error!(%error, "persisting a new webhook endpoint failed");
        return service_unavailable("webhook");
    }
    (
        StatusCode::CREATED,
        Json(RegisterWebhookResponse {
            id: id.to_string(),
            url: vetted.url,
            signing_secret: secret,
        }),
    )
        .into_response()
}

#[utoipa::path(
    get,
    path = "/admin/webhooks",
    extensions(("x-permission" = json!("console.data.read"))),
    security(("session_cookie" = [])),
    params(
        ("tenant_id" = String, Query, description = "The tenant whose endpoints to list (a 26-character ULID)"),
    ),
    responses(
        (status = 200, description = "The tenant's endpoints, as an array: each with `id`, \
                                      `store_id`, `url`, `cursor` (the last event delivered, absent \
                                      before the first) and `disabled`. Never the signing secret"),
        (status = 400, description = "tenant_id is absent or not a ULID", body = crate::openapi_admin::ErrorResponse),
        (status = 401, description = "No session", body = crate::openapi_admin::ErrorResponse),
        (status = 403, description = "The role lacks console.data.read", body = crate::openapi_admin::ErrorResponse),
        (status = 503, description = "The endpoint store is unreachable", body = crate::openapi_admin::ErrorResponse),
    ),
    tag = "integrator access",
)]
/// Lists a tenant's webhook endpoints as metadata only — never a secret (super-admin only).
pub(in crate::http) async fn admin_list_webhooks<S, R, K, C, A, T, W>(
    State(app): State<CloudApp<S, R, K, C, A, T, W>>,
    headers: HeaderMap,
    Query(query): Query<WebhookTenantQuery>,
) -> Response
where
    S: Clone + Send + Sync + 'static,
    R: Clone + Send + Sync + 'static,
    K: Clone + Send + Sync + 'static,
    C: ClockSource + Clone + Send + Sync + 'static,
    A: AdminStore + Clone + Send + Sync + 'static,
    T: Clone + Send + Sync + 'static,
    W: WebhookEndpointStore + Clone + Send + Sync + 'static,
{
    if let Err(denied) =
        require_permission(&app.admin, &app.clock, &headers, ConsolePermission::Read).await
    {
        return denied;
    }
    let tenant_id = match parse_ulid_fields([("tenant_id", &query.tenant_id)]) {
        Ok([tenant_id]) => TenantId::new(tenant_id),
        Err(refusal) => return refusal,
    };
    match app.webhooks.list_for_tenant(tenant_id).await {
        Ok(summaries) => (StatusCode::OK, Json::<Vec<WebhookSummary>>(summaries)).into_response(),
        Err(error) => {
            tracing::error!(%error, "listing webhook endpoints failed");
            service_unavailable("webhook")
        }
    }
}

#[utoipa::path(
    delete,
    path = "/admin/webhooks/{id}",
    extensions(("x-permission" = json!("console.webhooks.manage"))),
    security(("session_cookie" = [])),
    params(
        ("id" = String, Path, description = "The endpoint's 26-character ULID"),
        ("tenant_id" = String, Query, description = "The tenant that owns it (a 26-character ULID)"),
    ),
    responses(
        (status = 204, description = "The tenant has no endpoint with that id any more. Idempotent: \
                                      the answer is the same whether one was found, and another \
                                      tenant's endpoint is never removed"),
        (status = 400, description = "tenant_id or id is absent or not a ULID", body = crate::openapi_admin::ErrorResponse),
        (status = 401, description = "No session", body = crate::openapi_admin::ErrorResponse),
        (status = 403, description = "The role lacks console.webhooks.manage", body = crate::openapi_admin::ErrorResponse),
        (status = 503, description = "The endpoint store is unreachable", body = crate::openapi_admin::ErrorResponse),
    ),
    tag = "integrator access",
)]
/// Deletes a webhook endpoint by id within a tenant (super-admin only). `204` whether or not a live
/// endpoint was found — deletion is idempotent, the tenant scope stops one tenant removing another's
/// endpoint, and reporting which case it was is a needless enumeration signal.
pub(in crate::http) async fn admin_delete_webhook<S, R, K, C, A, T, W>(
    State(app): State<CloudApp<S, R, K, C, A, T, W>>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Query(query): Query<WebhookTenantQuery>,
) -> Response
where
    S: Clone + Send + Sync + 'static,
    R: Clone + Send + Sync + 'static,
    K: Clone + Send + Sync + 'static,
    C: ClockSource + Clone + Send + Sync + 'static,
    A: AdminStore + Clone + Send + Sync + 'static,
    T: Clone + Send + Sync + 'static,
    W: WebhookEndpointStore + Clone + Send + Sync + 'static,
{
    if let Err(denied) = require_permission(
        &app.admin,
        &app.clock,
        &headers,
        ConsolePermission::ManageWebhooks,
    )
    .await
    {
        return denied;
    }
    let (tenant_id, id) = match parse_ulid_fields([("tenant_id", &query.tenant_id), ("id", &id)]) {
        Ok([tenant_id, id]) => (TenantId::new(tenant_id), WebhookEndpointId::new(id)),
        Err(refusal) => return refusal,
    };
    match app.webhooks.delete(tenant_id, id).await {
        Ok(_removed) => StatusCode::NO_CONTENT.into_response(),
        Err(error) => {
            tracing::error!(%error, "deleting a webhook endpoint failed");
            service_unavailable("webhook")
        }
    }
}

#[utoipa::path(
    post,
    path = "/admin/webhooks/{id}/enable",
    extensions(("x-permission" = json!("console.webhooks.manage"))),
    security(("session_cookie" = [])),
    params(
        ("id" = String, Path, description = "The endpoint's 26-character ULID"),
        ("tenant_id" = String, Query, description = "The tenant that owns it (a 26-character ULID)"),
    ),
    responses(
        (status = 204, description = "Enabled. Delivery resumes from the endpoint's stored cursor, so \
                                      nothing queued while it was disabled is skipped. Enabling an \
                                      endpoint already enabled changes nothing"),
        (status = 400, description = "tenant_id or id is absent or not a ULID", body = crate::openapi_admin::ErrorResponse),
        (status = 401, description = "No session", body = crate::openapi_admin::ErrorResponse),
        (status = 403, description = "The role lacks console.webhooks.manage", body = crate::openapi_admin::ErrorResponse),
        (status = 404, description = "The tenant has no endpoint with that id", body = crate::openapi_admin::ErrorResponse),
        (status = 503, description = "The endpoint store is unreachable", body = crate::openapi_admin::ErrorResponse),
    ),
    tag = "integrator access",
)]
/// Re-enables a webhook endpoint that a day of continuous failure had auto-disabled (super-admin
/// only). Delivery resumes from the endpoint's stored cursor, so nothing that queued while it was
/// down is skipped ([ADR-0032](../../../docs/adr/0032-webhooks.md)).
///
/// Tenant-scoped like deletion, but enforced differently: [`WebhookEndpointStore::set_disabled`]
/// addresses an endpoint by id alone — the delivery task, which also calls it, holds no tenant — so
/// the scope is checked here by confirming the id appears in this tenant's own listing before the
/// flag is cleared. An id that is not this tenant's is a `404`, never a cross-tenant write. Idempotent:
/// re-enabling an already-active endpoint is a no-op `204`.
pub(in crate::http) async fn admin_enable_webhook<S, R, K, C, A, T, W>(
    State(app): State<CloudApp<S, R, K, C, A, T, W>>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Query(query): Query<WebhookTenantQuery>,
) -> Response
where
    S: Clone + Send + Sync + 'static,
    R: Clone + Send + Sync + 'static,
    K: Clone + Send + Sync + 'static,
    C: ClockSource + Clone + Send + Sync + 'static,
    A: AdminStore + Clone + Send + Sync + 'static,
    T: Clone + Send + Sync + 'static,
    W: WebhookEndpointStore + Clone + Send + Sync + 'static,
{
    if let Err(denied) = require_permission(
        &app.admin,
        &app.clock,
        &headers,
        ConsolePermission::ManageWebhooks,
    )
    .await
    {
        return denied;
    }
    let (tenant_id, endpoint_id) =
        match parse_ulid_fields([("tenant_id", &query.tenant_id), ("id", &id)]) {
            Ok([tenant_id, endpoint_id]) => (
                TenantId::new(tenant_id),
                WebhookEndpointId::new(endpoint_id),
            ),
            Err(refusal) => return refusal,
        };
    // Confirm the endpoint is this tenant's before clearing its flag: `set_disabled` is not itself
    // tenant-scoped, so the scope is enforced here against the tenant's own listing.
    match app.webhooks.list_for_tenant(tenant_id).await {
        Ok(summaries) => {
            if !summaries
                .iter()
                .any(|summary| summary.id == endpoint_id.to_string())
            {
                return not_found("webhook endpoint for this tenant");
            }
        }
        Err(error) => {
            tracing::error!(%error, "listing webhook endpoints failed");
            return service_unavailable("webhook");
        }
    }
    match app.webhooks.set_disabled(endpoint_id, false).await {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(error) => {
            tracing::error!(%error, "re-enabling a webhook endpoint failed");
            service_unavailable("webhook")
        }
    }
}

/// Mints a webhook endpoint id from `now_ms`, or `None` if the OS entropy source is unavailable.
fn mint_webhook_id(now_ms: i64) -> Option<WebhookEndpointId> {
    mint_ulid(now_ms).map(WebhookEndpointId::new)
}
