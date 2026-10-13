// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The till names the X/Z report shows, read from each store's published `devices` node.

use pos_proto::devices::PublishedDevices;
use pos_proto::ids::{StoreId, TenantId};

use crate::cloud::{DailyCash, STORE_DRAWER};
use crate::config_tree::{CapabilityValidator, ConfigTree, ConfigTreeStore};
use crate::http::strip_tree_version;
use crate::reports::till_names::TillNames;

/// The reports' [`TillNames`] over the stores' configuration trees.
#[derive(Debug, Clone)]
pub(crate) struct PublishedTillNames<T>(pub(crate) T);

impl<T: ConfigTreeStore + Sync> TillNames for PublishedTillNames<T> {
    async fn till_names(
        &self,
        store: (TenantId, StoreId),
        cash: &DailyCash,
    ) -> std::collections::BTreeMap<String, String> {
        published_till_names(&self.0, store, cash).await
    }
}

/// The names the store's published `devices` node gives the tills among `cash`'s drawers, by
/// `terminal_device_id` (ADR-0167 decision 13).
///
/// Read as the report is served, so a till renamed since shows its new name and one the node no
/// longer lists shows none, and the console labels it by its id. Only `TERMINAL` entries name a
/// drawer. A day with only the store's drawer reads nothing, and a tree that cannot be read names
/// nothing rather than failing the report, whose figures stand without the names.
async fn published_till_names<T: ConfigTreeStore>(
    config_trees: &T,
    (tenant_id, store_id): (TenantId, StoreId),
    cash: &DailyCash,
) -> std::collections::BTreeMap<String, String> {
    if cash.by_drawer.keys().all(|drawer| drawer == STORE_DRAWER) {
        return std::collections::BTreeMap::new();
    }
    let Ok(Some(state)) = config_trees
        .load(tenant_id, store_id)
        .await
        .map(strip_tree_version)
    else {
        return std::collections::BTreeMap::new();
    };
    let devices = ConfigTree::from_state(store_id, CapabilityValidator, state)
        .current_effective()
        .and_then(|effective| effective.get("devices").cloned())
        .and_then(|node| serde_json::from_value::<PublishedDevices>(node).ok())
        .unwrap_or_default();
    devices
        .devices()
        .iter()
        .filter(|device| device.kind.known() == pos_proto::devices::DeviceKind::Terminal)
        .map(|till| (till.device_id.to_string(), till.name.as_str().to_owned()))
        .filter(|(till, _)| cash.by_drawer.contains_key(till))
        .collect()
}
