// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! Integrations: the vendor catalogue, the connections a tenant makes to vendors and their publish
//! to a store (ADR-0153), and webhook endpoints.
//!
//! The connection state, request body and its checks, audit summary and router are here.

use std::collections::BTreeSet;
use std::sync::Arc;

use axum::Router;
use axum::response::Response;
use axum::routing::get;
use connections::{
    admin_create_connection, admin_delete_connection, admin_get_connection, admin_list_connections,
    admin_update_connection, connection_error_response,
};
use pos_cloud_api::response::{
    api_error, api_error_with_details, parse_ulid_fields, service_unavailable,
};
use pos_proto::ErrorStatus;
use pos_proto::determinism::ClockSource;
use pos_proto::ids::TenantId;
use serde::Deserialize;

use crate::audit::AuditRecorder;
use crate::auth::admin::AdminStore;
use crate::http::FieldRefusal;

mod connections;
mod providers;
mod publish;
mod webhooks;

pub use providers::provider_catalogue_router;
pub use publish::config_integrations_router;
// The webhook handlers the main router mounts.
pub(super) use webhooks::{
    admin_delete_webhook, admin_enable_webhook, admin_list_webhooks, admin_register_webhook,
};
// The handlers the console's OpenAPI document names.
pub(crate) use connections::{
    __path_admin_create_connection, __path_admin_delete_connection, __path_admin_get_connection,
    __path_admin_list_connections, __path_admin_update_connection,
};
pub(crate) use providers::__path_admin_list_providers;
pub(crate) use publish::__path_admin_publish_integrations;

// --- Vendor connections (`/admin/integrations/connections`, ADR-0153) ---------------------------

/// The collaborators the connection routes need: the store, the catalogue a connection is checked
/// against, the key its secrets are sealed under (absent when `cloud.toml` has none), and the
/// admin, clock and audit every write carries.
#[derive(Clone)]
struct ConnectionState<S, A, C> {
    connections: S,
    providers: Arc<pos_providers::Registry>,
    integration_secret: Option<crate::connections::ConnectionSecret>,
    admin: A,
    clock: C,
    audit: Arc<dyn AuditRecorder>,
}

/// A create/update body for a connection. The id is server-owned: minted on create, the path id on
/// update.
///
/// `secrets` holds only the values this write sets. A secret field left out keeps the value the
/// connection already holds, because the console cannot show it to be sent back. `clear_secrets`
/// removes a stored value on purpose. Switching `provider_id` drops every stored secret: one
/// vendor's key is never another's, even when both call the field `api_key`.
#[derive(Debug, Clone, Deserialize)]
struct ConnectionRequest {
    tenant_id: String,
    provider_id: String,
    scope_level: crate::connections::ScopeLevel,
    #[serde(default)]
    scope_id: Option<String>,
    display_name: String,
    /// Absent means enabled: a caller that omits it is setting up a connection to use.
    #[serde(default = "connection_is_enabled")]
    enabled: bool,
    #[serde(default)]
    settings: pos_providers::Settings,
    #[serde(default)]
    secrets: std::collections::BTreeMap<String, String>,
    #[serde(default)]
    clear_secrets: Vec<String>,
}

/// The default for [`ConnectionRequest::enabled`].
const fn connection_is_enabled() -> bool {
    true
}

/// The longest name the console may give a connection.
const CONNECTION_NAME_MAX_CHARS: usize = 80;

/// What the audit trail keeps of a connection: everything but its secrets, and which secrets it
/// holds. A trail that recorded the values would be a second, unsealed copy of every credential.
#[derive(Debug, Clone, serde::Serialize)]
struct ConnectionAuditSummary {
    connection_id: String,
    provider_id: String,
    scope_level: crate::connections::ScopeLevel,
    scope_id: Option<String>,
    display_name: String,
    enabled: bool,
    setting_keys: Vec<String>,
    secrets_set: Vec<String>,
}

impl ConnectionAuditSummary {
    fn of(connection: &crate::connections::Connection) -> Self {
        Self {
            connection_id: connection.connection_id.clone(),
            provider_id: connection.provider_id.clone(),
            scope_level: connection.scope_level,
            scope_id: connection.scope_id.clone(),
            display_name: connection.display_name.clone(),
            enabled: connection.enabled,
            setting_keys: connection.settings.keys().cloned().collect(),
            secrets_set: connection.secrets_set().into_iter().collect(),
        }
    }
}

/// Builds the connection sub-router
/// ([ADR-0153](../../../../docs/adr/0153-a-vendor-is-a-provider-the-cloud-chooses.md) decisions 3–4).
///
/// Per-record CRUD over a tenant's vendor connections. Reads are behind [`ConsolePermission::Read`](crate::auth::console_rbac::ConsolePermission::Read)
/// and answer no secret value, ever — only which secret fields are set. Writes are behind
/// [`ConsolePermission::ManageIntegrations`](crate::auth::console_rbac::ConsolePermission::ManageIntegrations), check the settings against the provider's own schema,
/// and seal every secret before the store sees it.
pub fn connection_router<S, A, C>(
    connections: S,
    providers: Arc<pos_providers::Registry>,
    integration_secret: Option<crate::connections::ConnectionSecret>,
    admin: A,
    clock: C,
    audit: Arc<dyn AuditRecorder>,
) -> Router
where
    S: crate::connections::ConnectionStore + Clone + Send + Sync + 'static,
    A: AdminStore + Clone + Send + Sync + 'static,
    C: ClockSource + Clone + Send + Sync + 'static,
{
    Router::new()
        .route(
            "/admin/integrations/connections",
            get(admin_list_connections::<S, A, C>).post(admin_create_connection::<S, A, C>),
        )
        .route(
            "/admin/integrations/connections/{connection_id}",
            get(admin_get_connection::<S, A, C>)
                .put(admin_update_connection::<S, A, C>)
                .delete(admin_delete_connection::<S, A, C>),
        )
        .with_state(ConnectionState {
            connections,
            providers,
            integration_secret,
            admin,
            clock,
            audit,
        })
}

/// The brand or store a connection's scope names, checked against its level: none for the tenant,
/// a ULID for a brand or a store.
#[expect(
    clippy::result_large_err,
    reason = "the Err is an axum Response by design — it *is* the 400 the caller returns"
)]
fn connection_scope_id(request: &ConnectionRequest) -> Result<Option<String>, Response> {
    use crate::connections::ScopeLevel;

    match (request.scope_level, request.scope_id.as_deref()) {
        (ScopeLevel::Tenant, None) => Ok(None),
        (ScopeLevel::Tenant, Some(_)) => Err(FieldRefusal::new(
            "a tenant-wide connection names no brand or store",
            "scope_id",
            "NOT_ALLOWED",
        )
        .into_response()),
        (ScopeLevel::Brand | ScopeLevel::Store, scope_id) => {
            parse_ulid_fields([("scope_id", scope_id.unwrap_or_default())])
                .map(|[id]| Some(id.to_string()))
        }
    }
}

/// Seals each secret a write sets into `sealed`, bound to this tenant, connection and field.
///
/// No key, no secret: storing one unsealed as a fallback is exactly what this refuses to do.
#[expect(
    clippy::result_large_err,
    reason = "the Err is an axum Response by design — it *is* the 503 the caller returns"
)]
fn seal_connection_secrets(
    secrets: &std::collections::BTreeMap<String, String>,
    tenant_id: TenantId,
    connection_id: &str,
    integration_secret: Option<&crate::connections::ConnectionSecret>,
    sealed: &mut std::collections::BTreeMap<String, String>,
) -> Result<(), Response> {
    use crate::connections::{SealedFor, seal};

    if secrets.is_empty() {
        return Ok(());
    }
    let Some(key) = integration_secret else {
        return Err(api_error(
            ErrorStatus::Unavailable,
            "this cloud has no integration_secret in cloud.toml, so it cannot store a credential: \
             add one (openssl rand -hex 32) and restart",
        ));
    };
    for (field_key, value) in secrets {
        let sealed_for = SealedFor {
            tenant_id,
            connection_id,
            field_key,
        };
        match seal(key, sealed_for, value) {
            Ok(value) => {
                sealed.insert(field_key.clone(), value);
            }
            Err(error) => {
                tracing::error!(%error, "a connection secret could not be sealed");
                return Err(service_unavailable("integration secret"));
            }
        }
    }
    Ok(())
}

/// Checks a connection request and builds the record to store, sealing its new secrets.
///
/// `existing` is the connection as stored, on an update: the secrets it holds for the same provider
/// are kept unless the request replaces or clears them.
#[expect(
    clippy::result_large_err,
    reason = "the Err is an axum Response by design — it *is* the refusal the caller returns"
)]
fn build_connection(
    request: &ConnectionRequest,
    tenant_id: TenantId,
    connection_id: &str,
    providers: &pos_providers::Registry,
    integration_secret: Option<&crate::connections::ConnectionSecret>,
    existing: Option<&crate::connections::Connection>,
) -> Result<crate::connections::Connection, Response> {
    use crate::connections::Connection;

    let display_name = request.display_name.trim();
    if display_name.is_empty() {
        return Err(FieldRefusal::new(
            "a connection needs a name the console can show",
            "display_name",
            "REQUIRED",
        )
        .into_response());
    }
    if display_name.chars().count() > CONNECTION_NAME_MAX_CHARS {
        return Err(FieldRefusal::new(
            "a connection's name is at most 80 characters",
            "display_name",
            "TOO_LONG",
        )
        .into_response());
    }
    let Some(provider) = providers.get(&request.provider_id) else {
        return Err(FieldRefusal::new(
            "that provider is not one this cloud was built with",
            "provider_id",
            "PROVIDER_UNKNOWN",
        )
        .into_response());
    };
    let scope_id = connection_scope_id(request)?;

    // What the connection already holds sealed, for the same provider only.
    let mut sealed = existing
        .filter(|existing| existing.provider_id == request.provider_id)
        .map(|existing| existing.sealed_secrets.clone())
        .unwrap_or_default();
    for key in &request.clear_secrets {
        if !matches!(
            provider.field(key).map(|field| &field.kind),
            Some(pos_providers::FieldKind::Secret { .. })
        ) {
            return Err(FieldRefusal::new(
                "only a secret field of this provider can be cleared",
                "clear_secrets",
                "SETTING_NOT_SECRET",
            )
            .into_response());
        }
        sealed.remove(key);
    }
    let stored: BTreeSet<String> = sealed.keys().cloned().collect();
    if let Err(errors) = provider.check(&request.settings, &request.secrets, &stored) {
        let details: Vec<(String, &str)> = errors
            .iter()
            .map(|error| (format!("settings.{}", error.key()), error.reason()))
            .collect();
        let details: Vec<(&str, &str)> = details
            .iter()
            .map(|(field, reason)| (field.as_str(), *reason))
            .collect();
        return Err(api_error_with_details(
            ErrorStatus::InvalidArgument,
            "the settings do not match what this provider needs",
            &details,
        ));
    }
    seal_connection_secrets(
        &request.secrets,
        tenant_id,
        connection_id,
        integration_secret,
        &mut sealed,
    )?;
    Ok(Connection {
        connection_id: connection_id.to_owned(),
        provider_id: request.provider_id.clone(),
        scope_level: request.scope_level,
        scope_id,
        display_name: display_name.to_owned(),
        enabled: request.enabled,
        settings: request.settings.clone(),
        sealed_secrets: sealed,
    })
}

/// Refuses a second enabled connection in a family a scope may have only one of — one e-invoice
/// provider files a store's legal invoices, and one ERP holds its books (ADR-0153).
async fn exclusive_scope_taken<S>(
    connections: &S,
    providers: &pos_providers::Registry,
    tenant_id: TenantId,
    candidate: &crate::connections::Connection,
) -> Result<Option<Response>, Response>
where
    S: crate::connections::ConnectionStore + Send + Sync,
{
    let Some(family) = providers
        .get(&candidate.provider_id)
        .map(|provider| provider.family)
    else {
        return Ok(None);
    };
    if !family.exclusive() || !candidate.enabled {
        return Ok(None);
    }
    let existing = connections
        .list(tenant_id)
        .await
        .map_err(|error| connection_error_response(&error))?;
    let taken = existing.iter().map(|row| &row.record).any(|other| {
        other.connection_id != candidate.connection_id
            && other.enabled
            && other.scope_level == candidate.scope_level
            && other.scope_id == candidate.scope_id
            && providers
                .get(&other.provider_id)
                .is_some_and(|provider| provider.family == family)
    });
    Ok(taken.then(|| {
        api_error_with_details(
            ErrorStatus::AlreadyExists,
            "this scope already has an enabled connection in a family it may have only one of: \
             disable that one first",
            &[("enabled", "CONNECTION_SCOPE_TAKEN")],
        )
    }))
}
