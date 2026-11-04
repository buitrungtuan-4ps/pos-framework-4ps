// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The config-tree handle a [`PostgresStore`] builds, and the hook a committed save tells
//! ([ADR-0195](../../../../docs/adr/0195-the-cloud-wakes-a-store-to-pull-its-config.md)).

use std::sync::Arc;

use pos_proto::ids::{ConfigVersionId, StoreId, TenantId};

use crate::config_trees::PostgresConfigTrees;
use crate::store::PostgresStore;

/// Told that a store's config tree was saved, after the write committed: the tenant, the store, and
/// the version the saved tree now ends at
/// ([ADR-0195](../../../../docs/adr/0195-the-cloud-wakes-a-store-to-pull-its-config.md): the cloud
/// wakes the store).
///
/// A save is one autocommit statement on its own pooled connection ([`PostgresConfigTrees::save_state`]
/// takes no transaction), so when the caller tells this, the row is committed and every other
/// connection reads it. The hook must not block: it runs on the saving request's task.
#[derive(Clone)]
pub struct ConfigSaved(Arc<dyn Fn(TenantId, StoreId, ConfigVersionId) + Send + Sync>);

impl ConfigSaved {
    /// A hook calling `hook`.
    pub fn new(hook: impl Fn(TenantId, StoreId, ConfigVersionId) + Send + Sync + 'static) -> Self {
        Self(Arc::new(hook))
    }

    /// Tells the hook that `store`'s tree now ends at `version`.
    pub fn notify(&self, tenant: TenantId, store: StoreId, version: ConfigVersionId) {
        (self.0)(tenant, store, version);
    }
}

impl core::fmt::Debug for ConfigSaved {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str("ConfigSaved")
    }
}

impl PostgresStore {
    /// The configuration-tree store over this pool ([ADR-0033](../../../../docs/adr/0033-config-tree.md)).
    ///
    /// A cheap handle sharing the same pool; `pos-cloud` implements its `ConfigTreeStore` seam over it.
    #[must_use]
    pub fn config_trees(&self) -> PostgresConfigTrees {
        PostgresConfigTrees::new(self.pool.clone(), self.config_saved.clone())
    }

    /// Has every config-tree handle built from here on tell `hook` after each committed save. Set
    /// once at startup, before the store is shared.
    #[must_use]
    pub fn with_config_saved(mut self, hook: ConfigSaved) -> Self {
        self.config_saved = Some(hook);
        self
    }
}
