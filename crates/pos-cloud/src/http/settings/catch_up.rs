// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! A store that waited for an edge release gets its package version once it reports one
//! ([ADR-0189](../../../../docs/adr/0189-a-store-runs-a-package-version-hq-pins.md) decision 5).
//!
//! The OTA report route hands each report's store to [`PackageCatchUp`]; a worker republishes the
//! store's settings only when its pinned version is not what its document carries and can now be
//! compiled in. Bounded: the queue holds [`CATCH_UP_QUEUE`] stores and drops a report beyond that,
//! because the next publish that reaches the store compiles it in anyway. A publish never makes a
//! report, so the two cannot feed each other.

use pos_cloud_config::package_pin::{ContentChange, SettingsPackages};
use pos_proto::determinism::ClockSource;
use pos_proto::ids::{StoreId, TenantId};
use tokio::sync::mpsc;

use super::SettingsState;
use super::package_status::running_version;
use super::publish::{TenantLayout, content_change, publish_settings_to};
use crate::config_tree::ConfigTreeStore;
use crate::registry::RegistryStore;
use crate::settings::SettingsStore;
use crate::store_groups::StoreGroupStore;

/// The most reported stores waiting for the worker.
pub const CATCH_UP_QUEUE: usize = 256;

/// Where the OTA report route hands a store that reported a release.
#[derive(Clone, Debug)]
pub struct PackageCatchUp {
    sender: mpsc::Sender<(TenantId, StoreId)>,
}

impl PackageCatchUp {
    /// A hand-off and the receiving end the worker reads.
    pub(super) fn channel() -> (Self, mpsc::Receiver<(TenantId, StoreId)>) {
        let (sender, receiver) = mpsc::channel(CATCH_UP_QUEUE);
        (Self { sender }, receiver)
    }

    /// Hands over a store that has just reported its release. Never waits: a full queue drops it.
    pub fn store_reported(&self, tenant_id: TenantId, store_id: StoreId) {
        if self.sender.try_send((tenant_id, store_id)).is_err() {
            tracing::warn!(
                %store_id,
                "the package catch-up queue is full or closed; the next publish reaching the store \
                 compiles its package"
            );
        }
    }
}

/// Reads stores from `receiver` until every hand-off is gone, catching each up.
pub(super) async fn run<St, Rg, Grp, Cfg, A, C>(
    state: SettingsState<St, Rg, Grp, Cfg, A, C>,
    mut receiver: mpsc::Receiver<(TenantId, StoreId)>,
) where
    St: SettingsStore + SettingsPackages,
    Rg: RegistryStore,
    Grp: StoreGroupStore,
    Cfg: ConfigTreeStore,
    C: ClockSource,
{
    while let Some((tenant_id, store_id)) = receiver.recv().await {
        catch_up(&state, tenant_id, store_id).await;
    }
}

/// Republishes one store's settings if its pinned version can now be compiled in and its document
/// does not carry it yet; does nothing otherwise.
async fn catch_up<St, Rg, Grp, Cfg, A, C>(
    state: &SettingsState<St, Rg, Grp, Cfg, A, C>,
    tenant_id: TenantId,
    store_id: StoreId,
) where
    St: SettingsStore + SettingsPackages,
    Rg: RegistryStore,
    Grp: StoreGroupStore,
    Cfg: ConfigTreeStore,
    C: ClockSource,
{
    let Ok(layout) = TenantLayout::load(&state.registry, &state.groups, tenant_id).await else {
        tracing::warn!(%store_id, "the store's layout could not be read to catch its package up");
        return;
    };
    let Some(placement) = layout.placement(store_id) else {
        return;
    };
    let Ok(values) = state.settings.list(tenant_id).await else {
        tracing::warn!(%store_id, "the settings could not be read to catch a package up");
        return;
    };
    let resolved = crate::settings::resolve_for_store(&values, &placement);
    let ContentChange::Set(content) =
        content_change(&state.settings, tenant_id, store_id, &resolved).await
    else {
        return;
    };
    let pinned = content.package_version_id.to_string();
    if running_version(&state.config_trees, tenant_id, store_id)
        .await
        .as_deref()
        == Some(&pinned)
    {
        return;
    }
    let results = publish_settings_to(state, tenant_id, &layout, &[store_id]).await;
    tracing::info!(
        %store_id,
        package_version_id = %pinned,
        published = results.len(),
        "a store that waited for its release now gets its package version"
    );
}
