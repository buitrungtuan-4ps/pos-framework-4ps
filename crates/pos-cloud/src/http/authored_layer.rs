// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The write of a whole authored layer, which `PUT /admin/stores/{store_id}/config/{level}` makes.

use axum::http::HeaderMap;
use axum::response::Response;

use pos_proto::ClockSource;
use pos_proto::ids::{ConfigVersionId, StoreId, TenantId};

use super::{
    LoadedTree, if_match_config, mint_version_id, save_if_confirmed, service_unavailable,
    unprocessable_violations,
};
use crate::config_tree::{
    CapabilityValidator, ConfigError, ConfigLevel, ConfigTree, ConfigTreeStore,
};

/// Publishes a whole authored `layer` at `level`, refusing a write made against a stale read.
///
/// This is the other half of [`publish_config_nodes`](super::publish_config_nodes), and the difference is what the caller is
/// doing. A node publish sets one key and commutes with the others, so it retries. This one replaces
/// a layer an operator *typed* — every key in it, including keys they never looked at — so a second
/// writer really does destroy work. That earns a refusal the operator has to see, carrying the
/// `ConfigVersionId` they were editing so the history screen can show them what changed.
///
/// Used only by `PUT /admin/config` and `POST /admin/config/versions/{id}/restore`.
pub(super) async fn publish_authored_layer<Cfg, C>(
    config_trees: &Cfg,
    clock: &C,
    headers: &HeaderMap,
    tenant_id: TenantId,
    store_id: StoreId,
    loaded: LoadedTree,
    level: ConfigLevel,
    layer: serde_json::Value,
) -> Result<ConfigVersionId, Response>
where
    Cfg: ConfigTreeStore,
    C: ClockSource,
{
    let mut tree = match loaded.state {
        Some(existing) => ConfigTree::from_state(store_id, CapabilityValidator, existing),
        None => ConfigTree::new(store_id, CapabilityValidator),
    };
    if_match_config(headers, tree.current_version())?;
    let Some(version_id) = mint_version_id(clock.now().as_milliseconds_since_epoch()) else {
        tracing::error!("could not read OS entropy to mint a config version id");
        return Err(service_unavailable("configuration"));
    };
    // A typed layer names no single node, so the version records the layer's own top-level keys:
    // that is what the operator replaced, and it is the best answer available without asking them
    // to annotate their own JSON.
    let node_keys: Vec<String> = layer
        .as_object()
        .map(|map| map.keys().cloned().collect())
        .unwrap_or_default();
    // What the store runs now, for the one check every write but the settings write takes
    // (ADR-0169 decision 3).
    let before = tree.effective();
    let id = match tree.publish(level, layer, version_id, node_keys) {
        Ok(id) => id,
        Err(ConfigError::Invalid(violations)) => return Err(unprocessable_violations(&violations)),
    };
    let saved = (tenant_id, store_id, loaded.version.as_ref());
    save_if_confirmed(config_trees, saved, &before, &tree).await?;
    Ok(id)
}
