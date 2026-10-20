// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! `PUT /admin/config/campaigns` and its preview: a tenant's campaigns written to a store's
//! `campaigns` node.

use std::sync::Arc;

use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use axum::{Json, Router};
use pos_cloud_api::console::{audit_action, require_permission};
use pos_cloud_api::response::{parse_ulid_fields, service_unavailable};
use pos_proto::determinism::ClockSource;
use pos_proto::ids::{StoreId, TenantId};
use serde::Deserialize;

use super::campaign_error_response;
use crate::audit::AuditRecorder;
use crate::auth::admin::AdminStore;
use crate::auth::console_rbac::ConsolePermission;
use crate::campaigns::{CampaignStore, to_node as campaigns_to_node};
use crate::config_tree::{ConfigLevel, ConfigTreeStore};
use crate::http::{
    PublishedConfig, config_redacted_for, load_tree_for_write, preview_config_node,
    publish_config_nodes, unprocessable_violations,
};
use crate::version::records;

// --- Campaign publish (`/admin/config/campaigns`, ADR-0077, Track M3) --------------------------

/// The collaborators the campaign-publish route needs: the campaign store the promotions are read
/// from, the config-tree store the `campaigns` node is written onto, plus the admin/clock/audit every
/// write carries.
#[derive(Clone)]
struct ConfigCampaignsState<Camp, Cfg, A, C> {
    campaigns: Camp,
    config_trees: Cfg,
    admin: A,
    clock: C,
    audit: Arc<dyn AuditRecorder>,
}

/// A `PUT /admin/config/campaigns` body: the `(tenant, store)` to push the tenant's authored campaigns
/// to. Like every other node publish, the campaigns come from what is authored, not the body.
#[derive(Debug, Clone, Deserialize)]
struct PublishCampaignsRequest {
    tenant_id: String,
    store_id: String,
}

/// Builds the campaign-publish sub-router ([ADR-0077](../../../docs/adr/0077-campaigns-and-scheduling.md), M3).
///
/// One route: assemble the tenant's authored campaigns into a `PublishedCampaigns`, write it as the
/// store's `campaigns` config node, and version it through the config tree — the same node-merge the
/// tax/floor/menu publishes use, so the other Store-level keys survive. Behind
/// [`ConsolePermission::PublishConfig`]. The edge applies the node to `EdgeSession::campaigns`.
pub fn config_campaigns_router<Camp, Cfg, A, C>(
    campaigns: Camp,
    config_trees: Cfg,
    admin: A,
    clock: C,
    audit: Arc<dyn AuditRecorder>,
) -> Router
where
    Camp: CampaignStore + Clone + Send + Sync + 'static,
    Cfg: ConfigTreeStore + Clone + Send + Sync + 'static,
    A: AdminStore + Clone + Send + Sync + 'static,
    C: ClockSource + Clone + Send + Sync + 'static,
{
    Router::new()
        .route(
            "/admin/config/campaigns",
            axum::routing::put(admin_publish_campaigns::<Camp, Cfg, A, C>),
        )
        .route(
            "/admin/config/campaigns/preview",
            post(preview_publish_campaigns::<Camp, Cfg, A, C>),
        )
        .with_state(ConfigCampaignsState {
            campaigns,
            config_trees,
            admin,
            clock,
            audit,
        })
}

/// Compiles a tenant's authored promotions into the store's `campaigns` node, with the count.
///
/// Shared with the batch fan-out ([ADR-0122](../../../docs/adr/0122-a-store-group-is-a-delivery-cohort.md) §3):
/// the node is the same at every member, so a batch compiles it once and writes it *N* times.
pub(in crate::http) async fn campaigns_nodes<Camp>(
    campaigns: &Camp,
    tenant_id: TenantId,
) -> Result<(Vec<(String, serde_json::Value)>, usize), Response>
where
    Camp: CampaignStore,
{
    let campaigns = match campaigns.list_campaigns(tenant_id).await {
        Ok(campaigns) => records(campaigns),
        Err(error) => return Err(campaign_error_response(&error)),
    };
    let count = campaigns.len();
    let Ok(campaigns_value) = serde_json::to_value(campaigns_to_node(&campaigns)) else {
        tracing::error!("could not serialise a campaigns node");
        return Err(service_unavailable("campaign"));
    };
    Ok((vec![("campaigns".to_owned(), campaigns_value)], count))
}

#[utoipa::path(
    put,
    path = "/admin/config/campaigns",
    security(("session_cookie" = [])),
    request_body(
        description = "`tenant_id` and `store_id`. The campaigns come from what the tenant has \
                       authored, never from the body",
        content_type = "application/json",
    ),
    responses(
        (status = 200, description = "Every campaign the tenant has authored is compiled into the \
                                      store's `campaigns` node and published on the Store layer, the \
                                      store's other Store-level settings kept. The body carries \
                                      `config_version_id`. Audited with the number of campaigns, \
                                      not their terms"),
        (status = 400, description = "tenant_id or store_id is not a ULID, or a value needs a \
                                      recorded confirmation first", body = crate::openapi_admin::ErrorResponse),
        (status = 401, description = "No session", body = crate::openapi_admin::ErrorResponse),
        (status = 403, description = "The role lacks console.config.publish", body = crate::openapi_admin::ErrorResponse),
        (status = 412, description = "Every attempt to publish lost a race with another publish: \
                                      try again", body = crate::openapi_admin::ErrorResponse),
        (status = 422, description = "The store's configuration would not validate with the node", body = crate::openapi_admin::ErrorResponse),
        (status = 503, description = "The campaign store or the configuration store is unreachable", body = crate::openapi_admin::ErrorResponse),
    ),
    tag = "campaigns",
)]
/// Assembles a tenant's authored campaigns into a `PublishedCampaigns`, writes it as the store's
/// `campaigns` node, and versions it — the same load→merge→publish→version shape as the other node
/// publishes.
async fn admin_publish_campaigns<Camp, Cfg, A, C>(
    State(state): State<ConfigCampaignsState<Camp, Cfg, A, C>>,
    headers: HeaderMap,
    Json(request): Json<PublishCampaignsRequest>,
) -> Response
where
    Camp: CampaignStore + Clone + Send + Sync + 'static,
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
    // Set the `campaigns` key on the store's Store layer (index 2) and re-publish it, preserving the
    // other Store-level keys (`menu`, `tax`, `permissions`, `floor`, capability flags).
    let (nodes, campaign_count) = match campaigns_nodes(&state.campaigns, tenant_id).await {
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
            "config.campaigns.publish",
            "store",
            &store_id.to_string(),
            None,
            serde_json::to_value(campaign_count).ok(),
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

#[utoipa::path(
    post,
    path = "/admin/config/campaigns/preview",
    security(("session_cookie" = [])),
    request_body(
        description = "`tenant_id` and `store_id`. The campaigns come from what the tenant has \
                       authored, never from the body. Nothing is written",
        content_type = "application/json",
    ),
    responses(
        (status = 200, description = "What publishing the campaigns would change: \
                                      `from_version_id`, the version the diff is against, or `null` \
                                      for a store never published to; `diff`, the merge patch \
                                      (RFC 7386) the publish would apply; and `unchanged`. The diff \
                                      never shows a staff PIN hash; shows a name or staff code \
                                      only to a role holding console.people.read; and shows the \
                                      campaigns, and any other priced node, only to a role holding \
                                      console.reports.revenue, so Ops, who may publish, is not \
                                      shown their terms. `unchanged` is computed before redaction. \
                                      Not audited"),
        (status = 400, description = "tenant_id or store_id is not a ULID", body = crate::openapi_admin::ErrorResponse),
        (status = 401, description = "No session", body = crate::openapi_admin::ErrorResponse),
        (status = 403, description = "The role lacks console.config.publish", body = crate::openapi_admin::ErrorResponse),
        (status = 422, description = "The publish would be refused, with its violations verbatim", body = crate::openapi_admin::ErrorResponse),
        (status = 503, description = "The campaign store or the configuration store is unreachable", body = crate::openapi_admin::ErrorResponse),
    ),
    tag = "campaigns",
)]
/// `POST /admin/config/campaigns/preview` — the dry-run of the campaigns publish. Assembles the
/// tenant's authored campaigns into the `campaigns` node exactly as the publish route would, then
/// returns the merge patch it would apply to the store's effective document — without minting a
/// version, saving, or auditing (it changes nothing). `422` with the same violations a real publish
/// would reject the candidate with. Behind [`ConsolePermission::PublishConfig`], the same audience
/// that can actually publish. The diff is redacted for the caller's role, as the generic preview's
/// is ([`config_redacted_for`]): its staff, because it can carry the `permissions` node too, and
/// its prices, because the `campaigns` node it previews carries combo prices, discount amounts and
/// minimum-bill thresholds — which Ops, who may publish, does not read (S8).
async fn preview_publish_campaigns<Camp, Cfg, A, C>(
    State(state): State<ConfigCampaignsState<Camp, Cfg, A, C>>,
    headers: HeaderMap,
    Json(request): Json<PublishCampaignsRequest>,
) -> Response
where
    Camp: CampaignStore + Clone + Send + Sync + 'static,
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
    let campaigns = match state.campaigns.list_campaigns(tenant_id).await {
        Ok(campaigns) => records(campaigns),
        Err(error) => return campaign_error_response(&error),
    };
    let Ok(campaigns_value) = serde_json::to_value(campaigns_to_node(&campaigns)) else {
        tracing::error!("could not serialise a campaigns node");
        return service_unavailable("campaign");
    };
    let loaded = match load_tree_for_write(&state.config_trees, tenant_id, store_id).await {
        Ok(loaded) => loaded,
        Err(refusal) => return refusal,
    };
    let state_before = &loaded.state;
    match preview_config_node(state_before.as_ref(), "campaigns", campaigns_value) {
        Ok(mut preview) => {
            preview.diff = config_redacted_for(&preview.diff, context.admin.role);
            (StatusCode::OK, Json(preview)).into_response()
        }
        Err(violations) => unprocessable_violations(&violations),
    }
}
