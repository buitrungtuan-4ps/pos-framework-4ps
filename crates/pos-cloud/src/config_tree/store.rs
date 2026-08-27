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

use pos_proto::ids::{ConfigVersionId, StoreId, TenantId};
use pos_proto::time::Timestamp;

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

    /// Records that a store contacted the cloud on its config pull
    /// ([ADR-0068](../../../docs/adr/0068-fleet-liveness.md)): the contact instant `seen_at` and the
    /// config version it reported holding (`None` if it holds nothing yet). This is the fleet-liveness
    /// read model's only write; the config pull is the liveness signal, so the seam that owns
    /// config-pull persistence records it. Callers treat it as best-effort telemetry and must not fail
    /// the pull the store needs if this write fails.
    ///
    /// # Errors
    ///
    /// [`ConfigStoreError`] if the liveness row could not be written.
    fn record_store_seen(
        &self,
        tenant: TenantId,
        store: StoreId,
        held_version: Option<ConfigVersionId>,
        seen_at: Timestamp,
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
