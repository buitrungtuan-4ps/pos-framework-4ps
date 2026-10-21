// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! `POST /admin/settings/publish`: a tenant's settings published to the stores they reach.

use axum::Json;
use axum::extract::State;
use axum::http::HeaderMap;
use axum::response::Response;
use pos_cloud_api::console::{audit_action, require_permission};
use pos_cloud_api::response::parse_ulid_fields;
use pos_proto::determinism::ClockSource;
use pos_proto::ids::{ConfigVersionId, StoreId, TenantId};

use super::presets::SettingsStoreRequest;
use super::{SettingsState, publish_results_response};
use crate::auth::admin::AdminStore;
use crate::auth::console_rbac::ConsolePermission;
use crate::config_tree::{ConfigLevel, ConfigTreeState, ConfigTreeStore};
use crate::http::setting_refusal::ConfirmCheck;
use crate::http::{load_tree_for_write, not_found, publish_nodes};
use crate::registry::RegistryStore;
use crate::settings::SettingsStore;
use crate::store_groups::StoreGroupStore;
// Moved to pos-cloud-config; imported here, so the settings routes keep reaching it.
pub(in crate::http) use pos_cloud_config::route::TenantLayout;

#[utoipa::path(
    post,
    path = "/admin/settings/publish",
    security(("session_cookie" = [])),
    extensions(("x-permission" = json!("console.config.publish"))),
    request_body(
        description = "The tenant, and optionally one store. Without a store, every store of the \
                       tenant is republished",
        content_type = "application/json",
    ),
    responses(
        (status = 200, description = "Each store's resolved settings republished, with the same \
                                      per-store outcomes as a write"),
        (status = 400, description = "tenant_id or store_id is not a ULID", body = crate::openapi_admin::ErrorResponse),
        (status = 401, description = "No session", body = crate::openapi_admin::ErrorResponse),
        (status = 403, description = "The role lacks console.config.publish", body = crate::openapi_admin::ErrorResponse),
        (status = 404, description = "The tenant has no such store", body = crate::openapi_admin::ErrorResponse),
        (status = 503, description = "The settings store, the registry, the store groups or the \
                                      config tree is unreachable", body = crate::openapi_admin::ErrorResponse),
    ),
    tag = "settings",
)]
/// `POST /admin/settings/publish` — republishes the settings of one store, or of every store of the
/// tenant.
///
/// A write republishes every store it reaches by itself. What it cannot see is a store that moved:
/// a store given another brand, or added to or removed from a store group, keeps the values it was
/// last published until this runs for it, as a group's batch publish works
/// ([ADR-0122](../../../docs/adr/0122-a-store-group-is-a-delivery-cohort.md)).
pub(super) async fn admin_publish_settings<St, Rg, Grp, Cfg, A, C>(
    State(state): State<SettingsState<St, Rg, Grp, Cfg, A, C>>,
    headers: HeaderMap,
    Json(request): Json<SettingsStoreRequest>,
) -> Response
where
    St: SettingsStore + Clone + Send + Sync + 'static,
    Rg: RegistryStore + Clone + Send + Sync + 'static,
    Grp: StoreGroupStore + Clone + Send + Sync + 'static,
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
    let tenant_id = match parse_ulid_fields([("tenant_id", &request.tenant_id)]) {
        Ok([tenant_id]) => TenantId::new(tenant_id),
        Err(refusal) => return refusal,
    };
    let layout = match TenantLayout::load(&state.registry, &state.groups, tenant_id).await {
        Ok(layout) => layout,
        Err(refusal) => return refusal,
    };
    let stores: Vec<StoreId> = match request.store_id.as_deref().map(str::trim) {
        None | Some("") => layout.stores.iter().map(|store| store.store_id).collect(),
        Some(text) => {
            let store_id = match parse_ulid_fields([("store_id", text)]) {
                Ok([store_id]) => StoreId::new(store_id),
                Err(refusal) => return refusal,
            };
            if layout.placement(store_id).is_none() {
                return not_found("store");
            }
            vec![store_id]
        }
    };
    let results = publish_settings_to(&state, tenant_id, &layout, &stores).await;
    audit_action(
        &state.audit,
        &state.clock,
        &context,
        Some(tenant_id),
        "setting.publish",
        "tenant",
        &tenant_id.to_string(),
        None,
        Some(serde_json::json!({ "stores": stores.len() })),
    )
    .await;
    publish_results_response(&results)
}

/// How one store's settings publish went.
#[derive(Debug, Clone, serde::Serialize)]
pub(super) struct SettingPublishResult {
    store_id: String,
    /// `SETTING_PUBLISH_APPLIED`, `SETTING_PUBLISH_UNCHANGED` or `SETTING_PUBLISH_FAILED`.
    outcome: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    config_version_id: Option<String>,
}

/// Republishes each store's resolved settings onto its Tenant layer.
///
/// One store failing does not stop the others: each answers for itself, and a failed one is
/// retried with `POST /admin/settings/publish`. A store whose layer already holds what it resolves
/// to is not given a new version, so a write that changes nothing for a store does not make its
/// tills reload.
///
/// The values are read once, before the stores are written. Two writes racing for one store can
/// therefore leave it holding the earlier write's picture until its next publish; the values
/// themselves are stored correctly, and `POST /admin/settings/publish` brings the store level.
pub(super) async fn publish_settings_to<St, Rg, Grp, Cfg, A, C>(
    state: &SettingsState<St, Rg, Grp, Cfg, A, C>,
    tenant_id: TenantId,
    layout: &TenantLayout,
    stores: &[StoreId],
) -> Vec<SettingPublishResult>
where
    St: SettingsStore,
    Cfg: ConfigTreeStore,
    C: ClockSource,
{
    let values = match state.settings.list(tenant_id).await {
        Ok(values) => values,
        Err(error) => {
            tracing::error!(%error, "the settings store could not be read for a settings publish");
            return stores
                .iter()
                .map(|store_id| SettingPublishResult {
                    store_id: store_id.to_string(),
                    outcome: "SETTING_PUBLISH_FAILED",
                    config_version_id: None,
                })
                .collect();
        }
    };
    let mut results = Vec::with_capacity(stores.len());
    // Stores left with the permissions switch and no people node beside it, counted and never
    // named ([`crate::settings::switch_without_people`]).
    let mut switch_only = 0_usize;
    for store_id in stores {
        let resolved = layout
            .placement(*store_id)
            .map(|placement| crate::settings::resolve_for_store(&values, &placement))
            .unwrap_or_default();
        let published = publish_settings_to_store(state, tenant_id, *store_id, &resolved).await;
        if published.switch_without_people {
            switch_only = switch_only.saturating_add(1);
        }
        results.push(match published.version {
            Ok(Some(version)) => SettingPublishResult {
                store_id: store_id.to_string(),
                outcome: "SETTING_PUBLISH_APPLIED",
                config_version_id: Some(version.to_string()),
            },
            Ok(None) => SettingPublishResult {
                store_id: store_id.to_string(),
                outcome: "SETTING_PUBLISH_UNCHANGED",
                config_version_id: None,
            },
            Err(()) => SettingPublishResult {
                store_id: store_id.to_string(),
                outcome: "SETTING_PUBLISH_FAILED",
                config_version_id: None,
            },
        });
    }
    if switch_only > 0 {
        tracing::warn!(
            stores = switch_only,
            "the permissions switch reached stores with no people node beside it: an edge from \
             0.14.1 on keeps its roster there, and an older one empties it until the store's \
             people are published (ADR-0158, ADR-0160)"
        );
    }
    results
}

/// What one store's settings publish did.
struct StorePublish {
    /// The new version, `None` when the layer already held the store's settings, or `Err` when the
    /// store could not be read or refused the write.
    version: Result<Option<ConfigVersionId>, ()>,
    /// Whether the store is left with the permissions switch and no people node beside it.
    switch_without_people: bool,
}

/// Writes one store's resolved settings onto its Tenant layer: the new version, or `None` when the
/// layer already holds them.
#[expect(
    clippy::result_large_err,
    reason = "the composing closure's Err is an axum Response by design — the shape every other \
              node publish's closure carries — and this closure never takes the Err branch"
)]
async fn publish_settings_to_store<St, Rg, Grp, Cfg, A, C>(
    state: &SettingsState<St, Rg, Grp, Cfg, A, C>,
    tenant_id: TenantId,
    store_id: StoreId,
    resolved: &[crate::settings::ResolvedSetting],
) -> StorePublish
where
    Cfg: ConfigTreeStore,
    C: ClockSource,
{
    let tenant_layer = |tree: Option<&ConfigTreeState>| {
        tree.map_or(serde_json::Value::Null, |tree| {
            tree.layer(ConfigLevel::Tenant).clone()
        })
    };
    let Ok(loaded) = load_tree_for_write(&state.config_trees, tenant_id, store_id).await else {
        tracing::error!(%store_id, "a store's configuration could not be read for a settings publish");
        return StorePublish {
            version: Err(()),
            switch_without_people: false,
        };
    };
    let layer = tenant_layer(loaded.state.as_ref());
    let nodes = crate::settings::tenant_layer_nodes(&layer, resolved);
    let switch_without_people = crate::settings::switch_without_people(
        &nodes,
        loaded
            .state
            .as_ref()
            .map_or(&serde_json::Value::Null, |tree| {
                tree.layer(ConfigLevel::Store)
            }),
    );
    if nodes
        .iter()
        .all(|(node, value)| layer.get(node) == Some(value))
    {
        return StorePublish {
            version: Ok(None),
            switch_without_people,
        };
    }
    let version = match publish_nodes(
        &state.config_trees,
        &state.clock,
        (tenant_id, store_id, ConfigLevel::Tenant),
        ConfirmCheck::SettingsRepublish,
        |tree| {
            Ok(crate::settings::tenant_layer_nodes(
                &tenant_layer(tree),
                resolved,
            ))
        },
    )
    .await
    {
        Ok(version) => Ok(Some(version)),
        Err(_refused) => {
            tracing::warn!(%store_id, "a store's configuration refused a settings publish");
            Err(())
        }
    };
    StorePublish {
        version,
        switch_without_people,
    }
}
