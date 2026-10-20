// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! `PUT /admin/config/tax`: a tenant's tax rates written to a store's `tax` node (ADR-0074).

use std::sync::Arc;

use axum::Router;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use pos_cloud_api::console::{audit_action, require_permission};
use pos_cloud_api::response::{parse_ulid_fields, service_unavailable};
use pos_proto::determinism::ClockSource;
use pos_proto::ids::{StoreId, TenantId};
use serde::Deserialize;

use crate::audit::AuditRecorder;
use crate::auth::admin::AdminStore;
use crate::auth::console_rbac::ConsolePermission;
use crate::config_tree::{ConfigLevel, ConfigTreeStore};
use crate::http::tax_rates::tax_rate_error_response;
use crate::http::{Json, PublishedConfig, publish_config_nodes};
use crate::tax::{TaxRateStore, to_table};

// --- Tax publish (`/admin/config/tax`, ADR-0074, Track M4) --------------------------------------

/// The collaborators the tax-publish route needs: the tax-rate store the table is read from, the
/// config-tree store the `tax` node is written onto, plus the admin/clock/audit every write carries.
#[derive(Clone)]
struct ConfigTaxState<Tax, Cfg, A, C> {
    tax_rates: Tax,
    config_trees: Cfg,
    admin: A,
    clock: C,
    audit: Arc<dyn AuditRecorder>,
}

/// A super-admin publishes a tenant's authored tax rates to one of its stores: the `(tenant, store)`.
/// The rates come from the authored table, not the body — publishing is "push what is authored".
#[derive(Debug, Clone, Deserialize)]
struct PublishTaxRequest {
    tenant_id: String,
    store_id: String,
}

/// Builds the tax-publish sub-router ([ADR-0074](../../../docs/adr/0074-localization-and-tax.md), M4).
///
/// One route: assemble the tenant's authored `(tax class × channel)` rates into a `TaxRateTable`, write
/// it as the store's `tax` config node, and version it through the config tree — the node-merge the
/// catalog/floor/people publishes use, so the other Store-level keys survive. Behind
/// [`ConsolePermission::PublishConfig`]. The edge applies the node to `EdgeSession::tax_rates`.
pub fn config_tax_router<Tax, Cfg, A, C>(
    tax_rates: Tax,
    config_trees: Cfg,
    admin: A,
    clock: C,
    audit: Arc<dyn AuditRecorder>,
) -> Router
where
    Tax: TaxRateStore + Clone + Send + Sync + 'static,
    Cfg: ConfigTreeStore + Clone + Send + Sync + 'static,
    A: AdminStore + Clone + Send + Sync + 'static,
    C: ClockSource + Clone + Send + Sync + 'static,
{
    Router::new()
        .route(
            "/admin/config/tax",
            axum::routing::put(admin_publish_tax::<Tax, Cfg, A, C>),
        )
        .with_state(ConfigTaxState {
            tax_rates,
            config_trees,
            admin,
            clock,
            audit,
        })
}

/// Compiles a tenant's authored rates into the store's `tax` node, with the rate count for the trail.
///
/// A read, not a write: the publish composes the node from whatever the table holds now, so the
/// version it was read at is nothing this path can use. Shared with the batch fan-out
/// ([ADR-0122](../../../docs/adr/0122-a-store-group-is-a-delivery-cohort.md) §3) — the node is the
/// same at every member, so a batch compiles it once.
pub(in crate::http) async fn tax_nodes<Tax>(
    tax_rates: &Tax,
    tenant_id: TenantId,
) -> Result<(Vec<(String, serde_json::Value)>, usize), Response>
where
    Tax: TaxRateStore,
{
    let entries = match tax_rates.list_tax_rates(tenant_id).await {
        Ok((entries, _version)) => entries,
        Err(error) => return Err(tax_rate_error_response(&error)),
    };
    let count = entries.len();
    let Ok(tax_value) = serde_json::to_value(to_table(&entries)) else {
        tracing::error!("could not serialise a tax rate table");
        return Err(service_unavailable("tax-rate"));
    };
    Ok((vec![("tax".to_owned(), tax_value)], count))
}

#[utoipa::path(
    put,
    path = "/admin/config/tax",
    security(("session_cookie" = [])),
    request_body(
        description = "`tenant_id` and `store_id`, then nothing else: the tenant's whole tax-rate table, as `GET /admin/catalog/tax-rates` shows it, is \
                       compiled into the node",
        content_type = "application/json",
    ),
    responses(
        (status = 200, description = "Published as the store's `tax` node on its \
                                      Store layer. The body carries `config_version_id`, which the \
                                      store pulls on its next sync. Audited"),
        (status = 400, description = "tenant_id or store_id is not a ULID, or a value needs \
                                      a recorded confirmation first (ADR-0169)", body = crate::openapi_admin::ErrorResponse),
        (status = 401, description = "No session", body = crate::openapi_admin::ErrorResponse),
        (status = 403, description = "The role lacks console.config.publish", body = crate::openapi_admin::ErrorResponse),
        (status = 412, description = "Every attempt to write the store's configuration lost a race \
                                      with another publish: try again", body = crate::openapi_admin::ErrorResponse),
        (status = 422, description = "The store's configuration would not validate with the \
                                      node", body = crate::openapi_admin::ErrorResponse),
        (status = 503, description = "The tax rate store or the configuration store is unreachable", body = crate::openapi_admin::ErrorResponse),
    ),
    tag = "config nodes",
)]
/// Assembles a tenant's authored rates into a `TaxRateTable`, writes it as the store's `tax` node, and
/// versions it — the same load→merge→publish→version shape as the other node publishes.
async fn admin_publish_tax<Tax, Cfg, A, C>(
    State(state): State<ConfigTaxState<Tax, Cfg, A, C>>,
    headers: HeaderMap,
    Json(request): Json<PublishTaxRequest>,
) -> Response
where
    Tax: TaxRateStore + Clone + Send + Sync + 'static,
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
    // Set the `tax` key on the store's Store layer (index 2) and re-publish it, preserving the other
    // Store-level keys (`menu`, `layout`, `permissions`, `floor`, capability flags).
    let (nodes, rate_count) = match tax_nodes(&state.tax_rates, tenant_id).await {
        Ok(compiled) => compiled,
        Err(refusal) => return refusal,
    };

    let id = match publish_config_nodes(
        &state.config_trees,
        &state.clock,
        tenant_id,
        store_id,
        ConfigLevel::Store,
        nodes,
    )
    .await
    {
        Ok(id) => id,
        Err(refusal) => return refusal,
    };
    {
        audit_action(
            &state.audit,
            &state.clock,
            &context,
            Some(tenant_id),
            "config.tax.publish",
            "store",
            &store_id.to_string(),
            None,
            serde_json::to_value(rate_count).ok(),
        )
        .await;
        (
            StatusCode::OK,
            Json(PublishedConfig {
                config_version_id: id.to_string(),
            }),
        )
            .into_response()
    }
}
