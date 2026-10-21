// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! `PUT /admin/config/integrations`: a tenant's connections written to a store's node (ADR-0153).

use std::sync::Arc;

use axum::Router;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use pos_cloud_api::console::{audit_action, require_permission};
use pos_cloud_api::response::{api_error, parse_ulid_fields, service_unavailable};
use pos_proto::ErrorStatus;
use pos_proto::determinism::ClockSource;
use pos_proto::ids::{ConfigVersionId, StoreId, TenantId};
use pos_proto::integrations::PublishedIntegrations;
use serde::Deserialize;

use super::connections::connection_error_response;
use crate::audit::AuditRecorder;
use crate::auth::admin::AdminStore;
use crate::auth::console_rbac::ConsolePermission;
use crate::config_tree::{ConfigLevel, ConfigTreeStore};
use crate::http::config_publish::publish_config_nodes;
use crate::http::{Json, not_found};
use crate::registry::RegistryStore;

// --- Integrations publish (`/admin/config/integrations`, ADR-0153) ------------------------------

/// The collaborators the integrations publish needs: the connections and the catalogue they are
/// resolved against, the registry that says which brand a store belongs to, the config tree the node
/// is written onto, and the admin, clock and audit every write carries.
#[derive(Clone)]
struct ConfigIntegrationsState<S, Rg, Cfg, A, C> {
    connections: S,
    providers: Arc<pos_providers::Registry>,
    registry: Rg,
    config_trees: Cfg,
    admin: A,
    clock: C,
    audit: Arc<dyn AuditRecorder>,
}

/// A `PUT /admin/config/integrations` body: the store to publish to. What goes on it is resolved
/// from the tenant's connections, never sent.
#[derive(Debug, Clone, Deserialize)]
struct PublishIntegrationsRequest {
    tenant_id: String,
    store_id: String,
}

/// What the trail keeps of an integrations publish: which connections the store was given. Ids and
/// provider ids only — never a setting, and there is no secret to leave out.
#[derive(Debug, Clone, serde::Serialize)]
struct IntegrationsPublishSummary {
    connections: Vec<String>,
    providers: Vec<String>,
}

/// Builds the integrations-publish sub-router
/// ([ADR-0153](../../../docs/adr/0153-a-vendor-is-a-provider-the-cloud-chooses.md) decision 5).
///
/// One route: resolve which of the tenant's connections serve one store — its own, its brand's, its
/// tenant's, one per exclusive family — and write the `integrations` node onto that store's Store
/// layer, preserving every other key. Behind [`ConsolePermission::PublishConfig`], like every node
/// publish: authoring a connection is `console.integrations.manage`, sending one to a store is a
/// publish.
pub fn config_integrations_router<S, Rg, Cfg, A, C>(
    connections: S,
    providers: Arc<pos_providers::Registry>,
    registry: Rg,
    config_trees: Cfg,
    admin: A,
    clock: C,
    audit: Arc<dyn AuditRecorder>,
) -> Router
where
    S: crate::connections::ConnectionStore + Clone + Send + Sync + 'static,
    Rg: RegistryStore + Clone + Send + Sync + 'static,
    Cfg: ConfigTreeStore + Clone + Send + Sync + 'static,
    A: AdminStore + Clone + Send + Sync + 'static,
    C: ClockSource + Clone + Send + Sync + 'static,
{
    Router::new()
        .route(
            "/admin/config/integrations",
            axum::routing::put(admin_publish_integrations::<S, Rg, Cfg, A, C>),
        )
        .with_state(ConfigIntegrationsState {
            connections,
            providers,
            registry,
            config_trees,
            admin,
            clock,
            audit,
        })
}

#[utoipa::path(
    put,
    path = "/admin/config/integrations",
    security(("session_cookie" = [])),
    extensions(("x-permission" = json!("console.config.publish"))),
    request_body(
        description = "The tenant and the store to publish to. The node's content is resolved from \
                       the tenant's connections: the store's own, then its brand's, then the \
                       tenant's, with one per e-invoice and ERP family",
        content_type = "application/json",
    ),
    responses(
        (status = 200, description = "Published. The body names the config version and how many \
                                      connections the store now has"),
        (status = 400, description = "tenant_id or store_id is absent or not a ULID", body = crate::openapi_admin::ErrorResponse),
        (status = 401, description = "No session", body = crate::openapi_admin::ErrorResponse),
        (status = 403, description = "The role lacks console.config.publish", body = crate::openapi_admin::ErrorResponse),
        (status = 404, description = "The tenant has no such store", body = crate::openapi_admin::ErrorResponse),
        (status = 503, description = "The connection store, the registry or the config tree is \
                                      unreachable", body = crate::openapi_admin::ErrorResponse),
    ),
    tag = "integrations",
)]
/// Publishes the connections that serve one store as its `integrations` node.
async fn admin_publish_integrations<S, Rg, Cfg, A, C>(
    State(state): State<ConfigIntegrationsState<S, Rg, Cfg, A, C>>,
    headers: HeaderMap,
    Json(request): Json<PublishIntegrationsRequest>,
) -> Response
where
    S: crate::connections::ConnectionStore + Clone + Send + Sync + 'static,
    Rg: RegistryStore + Clone + Send + Sync + 'static,
    Cfg: ConfigTreeStore + Clone + Send + Sync + 'static,
    A: AdminStore + Clone + Send + Sync + 'static,
    C: ClockSource + Clone + Send + Sync + 'static,
{
    let context = match require_permission(
        &state.admin,
        &state.clock,
        &headers,
        ConsolePermission::PublishConfig,
    )
    .await
    {
        Ok(context) => context,
        Err(denied) => return denied,
    };
    let (tenant_id, store_id) = match parse_ulid_fields([
        ("tenant_id", &request.tenant_id),
        ("store_id", &request.store_id),
    ]) {
        Ok([tenant_id, store_id]) => (TenantId::new(tenant_id), StoreId::new(store_id)),
        Err(refusal) => return refusal,
    };
    let node = match resolved_node(&state, tenant_id, store_id).await {
        Ok(node) => node,
        Err(refusal) => return refusal,
    };
    let summary = IntegrationsPublishSummary::of(&node);
    let id = match write_integrations(&state, tenant_id, store_id, &node).await {
        Ok(id) => id,
        Err(refusal) => return refusal,
    };
    audit_action(
        &state.audit,
        &state.clock,
        &context,
        Some(tenant_id),
        "config.integrations.publish",
        "store",
        &store_id.to_string(),
        None,
        serde_json::to_value(&summary).ok(),
    )
    .await;
    (
        StatusCode::OK,
        Json(serde_json::json!({
            "config_version_id": id.to_string(),
            "integrations": summary.connections.len(),
        })),
    )
        .into_response()
}

impl IntegrationsPublishSummary {
    /// The connections `node` gives the store and the provider of each, in the node's order.
    fn of(node: &PublishedIntegrations) -> Self {
        Self {
            connections: node
                .integrations()
                .iter()
                .map(|integration| integration.connection_id.clone())
                .collect(),
            providers: node
                .integrations()
                .iter()
                .map(|integration| integration.provider_id.clone())
                .collect(),
        }
    }
}

/// The connections that serve the store: its own, its brand's, its tenant's, one per exclusive
/// family.
async fn resolved_node<S, Rg, Cfg, A, C>(
    state: &ConfigIntegrationsState<S, Rg, Cfg, A, C>,
    tenant_id: TenantId,
    store_id: StoreId,
) -> Result<PublishedIntegrations, Response>
where
    S: crate::connections::ConnectionStore,
    Rg: RegistryStore,
{
    let brand_id = store_brand(state, tenant_id, store_id).await?;
    let connections = match state.connections.list(tenant_id).await {
        Ok(rows) => rows.into_iter().map(|row| row.record).collect::<Vec<_>>(),
        Err(error) => return Err(connection_error_response(&error)),
    };
    Ok(crate::connections::resolve_for_store(
        &connections,
        &state.providers,
        &store_id.to_string(),
        brand_id.as_deref(),
    ))
}

/// The brand of the store being published to, which decides the brand-scoped connections that
/// serve it.
///
/// A store the tenant does not have is refused rather than published to: a node written for a
/// typo'd id is a node nobody reads, and the operator would believe the store had been told.
async fn store_brand<S, Rg, Cfg, A, C>(
    state: &ConfigIntegrationsState<S, Rg, Cfg, A, C>,
    tenant_id: TenantId,
    store_id: StoreId,
) -> Result<Option<String>, Response>
where
    Rg: RegistryStore,
{
    match state.registry.list_stores(tenant_id).await {
        Ok(stores) => match stores
            .into_iter()
            .find(|store| store.record.store_id == store_id)
        {
            Some(store) => Ok(store.record.brand_id.map(|brand| brand.to_string())),
            None => Err(not_found("store")),
        },
        Err(error) => {
            tracing::error!(%error, "the registry could not be read for an integrations publish");
            Err(service_unavailable("registry"))
        }
    }
}

/// Writes `node` as the store's `integrations` node and answers the version.
async fn write_integrations<S, Rg, Cfg, A, C>(
    state: &ConfigIntegrationsState<S, Rg, Cfg, A, C>,
    tenant_id: TenantId,
    store_id: StoreId,
    node: &PublishedIntegrations,
) -> Result<ConfigVersionId, Response>
where
    Cfg: ConfigTreeStore,
    C: ClockSource,
{
    let Ok(value) = serde_json::to_value(node) else {
        return Err(api_error(
            ErrorStatus::Internal,
            "the integrations node could not be encoded",
        ));
    };
    publish_config_nodes(
        &state.config_trees,
        &state.clock,
        tenant_id,
        store_id,
        ConfigLevel::Store,
        vec![("integrations".to_owned(), value)],
    )
    .await
}
