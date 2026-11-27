// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The till names the X/Z report shows, read from each store's published `devices` node.

use pos_proto::devices::PublishedDevices;
use pos_proto::ids::{StoreId, TenantId};

use crate::cloud::{DailyCash, DailyRevenue, STORE_DRAWER};
use crate::config_tree::{CapabilityValidator, ConfigTree, ConfigTreeStore};
use crate::http::{published_vendors, strip_tree_version};
use pos_cloud_reports::till_names::{ChannelNames, TillNames};
use pos_proto::order_channels::PublishedOrderChannels;

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

    async fn vendor_names(
        &self,
        (tenant_id, store_id): (TenantId, StoreId),
        revenue: &DailyRevenue,
    ) -> std::collections::BTreeMap<String, String> {
        let apps: std::collections::BTreeSet<&String> = revenue
            .by_channel
            .iter()
            .filter_map(|line| line.vendor_id.as_ref())
            .chain(revenue.by_app_tender.keys())
            .collect();
        if apps.is_empty() {
            return std::collections::BTreeMap::new();
        }
        let vendors = published_vendors(&self.0, tenant_id, store_id).await;
        apps.into_iter()
            .filter_map(|app| {
                let vendor = vendors.get(app.parse().ok()?)?;
                Some((app.clone(), vendor.display_name.as_str().to_owned()))
            })
            .collect()
    }

    async fn channel_names(
        &self,
        store: (TenantId, StoreId),
        revenue: &DailyRevenue,
    ) -> (ChannelNames, ChannelNames) {
        published_channel_names(&self.0, store, revenue).await
    }
}

/// The store's names for the order type and source tokens among `revenue`'s lines, from its
/// published `order_channels` node (ADR-0260 decision 6); nothing where it cannot be read.
async fn published_channel_names<T: ConfigTreeStore>(
    config_trees: &T,
    (tenant_id, store_id): (TenantId, StoreId),
    revenue: &DailyRevenue,
) -> (ChannelNames, ChannelNames) {
    if revenue.by_channel.is_empty() {
        return (ChannelNames::new(), ChannelNames::new());
    }
    let Ok(Some(state)) = config_trees
        .load(tenant_id, store_id)
        .await
        .map(strip_tree_version)
    else {
        return (ChannelNames::new(), ChannelNames::new());
    };
    let named = ConfigTree::from_state(store_id, CapabilityValidator, state)
        .current_effective()
        .and_then(|effective| effective.get(PublishedOrderChannels::NODE).cloned())
        .and_then(|node| serde_json::from_value::<PublishedOrderChannels>(node).ok())
        .unwrap_or_default()
        .names();
    let pick = |tokens: std::collections::BTreeSet<&String>| -> ChannelNames {
        tokens
            .into_iter()
            .filter_map(|token| {
                let by_language = named.get(token.as_str())?;
                Some((
                    token.clone(),
                    by_language
                        .iter()
                        .map(|(language, name)| (language.clone(), name.as_str().to_owned()))
                        .collect(),
                ))
            })
            .collect()
    };
    (
        pick(
            revenue
                .by_channel
                .iter()
                .map(|line| &line.order_type)
                .collect(),
        ),
        pick(revenue.by_channel.iter().map(|line| &line.source).collect()),
    )
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
