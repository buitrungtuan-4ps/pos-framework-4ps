// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The seam that persists a store's [`ConfigTree`](super::ConfigTree) state
//! ([ADR-0033](../../../docs/adr/0033-config-tree.md)).
//!
//! The engine ([`super::tree`]) is pure and holds a store's tree in memory; this is where that tree
//! is loaded from and saved to durable storage. A [`ConfigTreeState`] — the
//! four authored layers plus the published history — is what crosses the seam, so the whole tree
//! round-trips through one JSON document per `(tenant, store)`, keyed and RLS-isolated by tenant
//! exactly as the rollup read model is. A table in `store-postgres`; a fake in tests.

use core::future::Future;

use pos_proto::ids::{StoreId, TenantId};

use super::ConfigTreeState;

/// Persists and loads a store's config-tree state, keyed by `(tenant, store)`.
pub trait ConfigTreeStore {
    /// Loads a store's tree state, or `None` if it has never been published to.
    ///
    /// # Errors
    ///
    /// [`ConfigStoreError`] if the store itself could not be read (not for an absent tree, which is
    /// `Ok(None)`).
    fn load(
        &self,
        tenant: TenantId,
        store: StoreId,
    ) -> impl Future<Output = Result<Option<ConfigTreeState>, ConfigStoreError>> + Send;

    /// Persists a store's tree state, replacing any prior one.
    ///
    /// # Errors
    ///
    /// [`ConfigStoreError`] if the store could not be written.
    fn save(
        &self,
        tenant: TenantId,
        store: StoreId,
        state: &ConfigTreeState,
    ) -> impl Future<Output = Result<(), ConfigStoreError>> + Send;
}

/// A failure of the config-tree store itself — the database is unreachable, or a stored document
/// could not be decoded. Distinct from a validation refusal ([`ConfigError`](super::ConfigError)),
/// which is a verdict on an authored change rather than a storage fault.
#[derive(Debug, thiserror::Error)]
#[error("the config-tree store failed: {0}")]
pub struct ConfigStoreError(String);

impl ConfigStoreError {
    /// A store failure carrying a human-readable reason (for the server's log).
    #[must_use]
    pub fn new(message: impl Into<String>) -> Self {
        Self(message.into())
    }
}
