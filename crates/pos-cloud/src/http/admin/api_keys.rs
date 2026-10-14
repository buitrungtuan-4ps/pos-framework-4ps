// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! `/admin/api-keys`: provision, list and revoke a tenant's API keys.

use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use pos_cloud_api::console::{audit_action, require_permission};
use pos_cloud_api::response::{api_error_with_details, parse_ulid_fields, service_unavailable};
use pos_proto::ErrorStatus;
use pos_proto::determinism::ClockSource;
use pos_proto::ids::{StoreId, TenantId};
use serde::Deserialize;

use crate::auth::admin::AdminStore;
use crate::auth::apikey::{ApiKeyAdminStore, ApiKeyId, issue};
use crate::auth::console_rbac::ConsolePermission;
use crate::http::{CloudApp, Json, mint_api_key_id, parse_scopes, random_hex_32};

/// A super-admin request to provision a new API key for a tenant.
#[derive(Debug, Clone, Deserialize)]
pub(in crate::http) struct CreateApiKeyRequest {
    /// The tenant the key will act for (a 26-character ULID).
    tenant_id: String,
    /// The one store the key will act for (a ULID), or absent for a tenant-wide key.
    ///
    /// A store's own credential — the key its edge presents on `/sync/stores/{store_id}/…` — must
    /// name its store here: those routes serve one store's configuration, including its employee
    /// roster and PIN hashes, and refuse a key that is not bound to the store in the path (S1). A
    /// tenant-wide key stays right for an integration reading a whole tenant's rollups.
    #[serde(default)]
    store_id: Option<String>,
    /// The scopes to grant, as their wire names (`read_rollups`, …). Deny-by-default: only these.
    scopes: Vec<String>,
    /// An optional expiry, in milliseconds since the Unix epoch. Omit for a key that never expires.
    #[serde(default)]
    expires_at_ms: Option<i64>,
}

/// The one-time response to a provisioning request: the id, and the token shown exactly once.
#[derive(Debug, Clone, serde::Serialize)]
struct CreateApiKeyResponse {
    /// The key's public id (the ULID half of the token).
    id: String,
    /// The full `pos_<id>_<secret>` token. **Shown once** — only its hash is stored, so it cannot be
    /// recovered later.
    token: String,
}

/// The tenant a listing is scoped to.
#[derive(Debug, Clone, Deserialize)]
pub(in crate::http) struct ListApiKeysQuery {
    /// The tenant whose keys to list (a 26-character ULID).
    tenant_id: String,
}

/// Provisions a new scoped per-tenant API key and returns the one-time token (super-admin only).
///
/// Behind the session guard. Mints a CSPRNG id and secret at the edge, [`issue`]s the key, and
/// persists it (storing only the secret's hash). The token in the `201` body is the only time the
/// secret is visible.
pub(in crate::http) async fn admin_create_api_key<S, R, K, C, A, T, W>(
    State(app): State<CloudApp<S, R, K, C, A, T, W>>,
    headers: HeaderMap,
    Json(request): Json<CreateApiKeyRequest>,
) -> Response
where
    S: Clone + Send + Sync + 'static,
    R: Clone + Send + Sync + 'static,
    K: ApiKeyAdminStore + Clone + Send + Sync + 'static,
    C: ClockSource + Clone + Send + Sync + 'static,
    A: AdminStore + Clone + Send + Sync + 'static,
    T: Clone + Send + Sync + 'static,
    W: Clone + Send + Sync + 'static,
{
    let context = match require_permission(
        &app.admin,
        &app.clock,
        &headers,
        ConsolePermission::ManageApiKeys,
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
    let store_id = match request.store_id.as_deref() {
        None => None,
        Some(raw) => match parse_ulid_fields([("store_id", raw)]) {
            Ok([store_id]) => Some(StoreId::new(store_id)),
            Err(refusal) => return refusal,
        },
    };
    // Strict: an unknown scope name is a `400`, not a silent drop — the admin is granting explicitly,
    // so a typo must not quietly issue a key that authorises nothing.
    let scopes = match parse_scopes(&request.scopes) {
        Ok(scopes) => scopes,
        Err(unknown) => {
            return api_error_with_details(
                ErrorStatus::InvalidArgument,
                format!("unknown scope: {unknown}"),
                &[("scopes", "INVALID_ENUM_VALUE")],
            );
        }
    };
    let now_ms = app.clock.now().as_milliseconds_since_epoch();
    let (Some(id), Some(secret)) = (mint_api_key_id(now_ms), random_hex_32()) else {
        tracing::error!("could not read OS entropy to mint an API key");
        return service_unavailable("provisioning");
    };
    let expires_at = match request.expires_at_ms {
        Some(ms) => match pos_proto::time::Timestamp::from_milliseconds_since_epoch(ms) {
            Ok(timestamp) => Some(timestamp),
            Err(_) => {
                return api_error_with_details(
                    ErrorStatus::InvalidArgument,
                    "expires_at_ms is out of range",
                    &[("expires_at_ms", "OUT_OF_RANGE")],
                );
            }
        },
        None => None,
    };
    let (stored, token) = issue(id, tenant_id, store_id, scopes, &secret, expires_at);
    if let Err(error) = app.keys.insert(&stored).await {
        tracing::error!(%error, "persisting a new API key failed");
        return service_unavailable("provisioning");
    }
    // Records the grant, never the secret: the scopes and expiry are the auditable fact; the token
    // is shown once to the caller and never written to the trail.
    let after = serde_json::json!({
        "scopes": request.scopes,
        "expires_at_ms": request.expires_at_ms,
    });
    audit_action(
        &app.audit,
        &app.clock,
        &context,
        Some(tenant_id),
        "api_key.create",
        "api_key",
        &id.to_string(),
        None,
        Some(after),
    )
    .await;
    (
        StatusCode::CREATED,
        Json(CreateApiKeyResponse {
            id: id.to_string(),
            token,
        }),
    )
        .into_response()
}

/// Lists a tenant's API keys as metadata only — never a secret (super-admin only).
pub(in crate::http) async fn admin_list_api_keys<S, R, K, C, A, T, W>(
    State(app): State<CloudApp<S, R, K, C, A, T, W>>,
    headers: HeaderMap,
    Query(query): Query<ListApiKeysQuery>,
) -> Response
where
    S: Clone + Send + Sync + 'static,
    R: Clone + Send + Sync + 'static,
    K: ApiKeyAdminStore + Clone + Send + Sync + 'static,
    C: ClockSource + Clone + Send + Sync + 'static,
    A: AdminStore + Clone + Send + Sync + 'static,
    T: Clone + Send + Sync + 'static,
    W: Clone + Send + Sync + 'static,
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
    match app.keys.list_for_tenant(tenant_id).await {
        Ok(summaries) => (StatusCode::OK, Json(summaries)).into_response(),
        Err(error) => {
            tracing::error!(%error, "listing API keys failed");
            service_unavailable("provisioning")
        }
    }
}

/// Revokes an API key by id (super-admin only). `204` whether or not a live key was found — revoking
/// is idempotent, and telling the caller which it was is a needless enumeration signal.
pub(in crate::http) async fn admin_revoke_api_key<S, R, K, C, A, T, W>(
    State(app): State<CloudApp<S, R, K, C, A, T, W>>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Response
where
    S: Clone + Send + Sync + 'static,
    R: Clone + Send + Sync + 'static,
    K: ApiKeyAdminStore + Clone + Send + Sync + 'static,
    C: ClockSource + Clone + Send + Sync + 'static,
    A: AdminStore + Clone + Send + Sync + 'static,
    T: Clone + Send + Sync + 'static,
    W: Clone + Send + Sync + 'static,
{
    let context = match require_permission(
        &app.admin,
        &app.clock,
        &headers,
        ConsolePermission::ManageApiKeys,
    )
    .await
    {
        Ok(context) => context,
        Err(denied) => return denied,
    };
    let id = match parse_ulid_fields([("id", &id)]) {
        Ok([id]) => ApiKeyId::new(id),
        Err(refusal) => return refusal,
    };
    match app.keys.revoke(id).await {
        Ok(found) => {
            // Only a revoke that actually retired a live key is worth a trail entry; revoking an
            // already-gone id is a no-op `204` and records nothing. The revoke seam is by-id only, so
            // the entry has no tenant scope (a fleet-wide read still surfaces it).
            if found {
                audit_action(
                    &app.audit,
                    &app.clock,
                    &context,
                    None,
                    "api_key.revoke",
                    "api_key",
                    &id.to_string(),
                    None,
                    None,
                )
                .await;
            }
            StatusCode::NO_CONTENT.into_response()
        }
        Err(error) => {
            tracing::error!(%error, "revoking an API key failed");
            service_unavailable("provisioning")
        }
    }
}
