// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! What a config rollback writes into the settings so a later publish keeps it
//! ([ADR-0246](../../../../docs/adr/0246-a-setting-has-one-source-and-publish-and-rollback-take-a-scope.md)
//! decision 1): [`pos_cloud_config::rollback_hold`] says which values, this writes them.
//!
//! The rollback route sits on the application state, which carries no settings store, so it takes
//! this as one object-safe handle built over the settings routes' own stores.

use core::future::Future;
use core::pin::Pin;
use std::sync::Arc;

use axum::response::Response;
use pos_cloud_config::rollback_hold::rollback_holds;
use pos_cloud_config::route::settings_store_error_response;
use pos_proto::ids::{StoreId, TenantId};
use pos_proto::time::Timestamp;

use super::publish::TenantLayout;
use crate::registry::RegistryStore;
use crate::settings::{SettingsStore, resolve_for_store};
use crate::store_groups::StoreGroupStore;

/// The settings a rollback held at the store, and those it could not.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct HeldSettings {
    /// The settings written at the store's own scope, by key.
    pub written: Vec<String>,
    /// The settings the store's scope cannot hold, by key: the next settings publish puts the
    /// resolved value back for these.
    pub not_held: Vec<String>,
}

/// The future [`RollbackSettings::hold`] answers with.
pub type HoldFuture<'a> = Pin<Box<dyn Future<Output = Result<HeldSettings, Response>> + Send + 'a>>;

/// Writes the setting values a rollback restored as the store's own, so the settings resolve to
/// what the store now runs.
pub trait RollbackSettings: Send + Sync {
    /// Holds `restored`, the effective document a rollback gave `store_id`, in the settings.
    ///
    /// # Errors
    ///
    /// The `503` to answer with when the settings, the registry or the store groups fail.
    fn hold<'a>(
        &'a self,
        tenant_id: TenantId,
        store_id: StoreId,
        restored: &'a serde_json::Value,
        now: Timestamp,
    ) -> HoldFuture<'a>;
}

/// The settings, registry and store groups a rollback resolves and writes through.
struct SettingsHold<St, Rg, Grp> {
    settings: St,
    registry: Rg,
    groups: Grp,
}

/// The rollback's settings handle over the settings routes' stores.
pub fn rollback_settings<St, Rg, Grp>(
    settings: St,
    registry: Rg,
    groups: Grp,
) -> Arc<dyn RollbackSettings>
where
    St: SettingsStore + Send + Sync + 'static,
    Rg: RegistryStore + Send + Sync + 'static,
    Grp: StoreGroupStore + Send + Sync + 'static,
{
    Arc::new(SettingsHold {
        settings,
        registry,
        groups,
    })
}

impl<St, Rg, Grp> RollbackSettings for SettingsHold<St, Rg, Grp>
where
    St: SettingsStore + Send + Sync,
    Rg: RegistryStore + Send + Sync,
    Grp: StoreGroupStore + Send + Sync,
{
    fn hold<'a>(
        &'a self,
        tenant_id: TenantId,
        store_id: StoreId,
        restored: &'a serde_json::Value,
        now: Timestamp,
    ) -> HoldFuture<'a> {
        Box::pin(async move {
            let layout = TenantLayout::load(&self.registry, &self.groups, tenant_id).await?;
            // A store the registry no longer lists is reached by no setting, so none is held.
            let Some(placement) = layout.placement(store_id) else {
                return Ok(HeldSettings::default());
            };
            let values = self
                .settings
                .list(tenant_id)
                .await
                .map_err(|error| settings_store_error_response(&error))?;
            let resolved = resolve_for_store(&values, &placement);
            let holds = rollback_holds(restored, &resolved, &placement.store_id, now);
            let mut written = Vec::with_capacity(holds.writes.len());
            for value in &holds.writes {
                self.settings
                    .put(tenant_id, value)
                    .await
                    .map_err(|error| settings_store_error_response(&error))?;
                written.push(value.setting_key.clone());
            }
            Ok(HeldSettings {
                written,
                not_held: holds.not_held,
            })
        })
    }
}
