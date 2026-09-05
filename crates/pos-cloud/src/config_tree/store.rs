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
use crate::version::{UpdateOutcome, Version, Versioned};

/// Persists and loads a store's config-tree state, keyed by `(tenant, store)`.
pub trait ConfigTreeStore {
    /// Loads a store's tree state with the [`Version`] it was read at, or `None` if it has never
    /// been published to.
    ///
    /// # Errors
    ///
    /// [`ConfigStoreError`] if the store itself could not be read (not for an absent tree, which is
    /// `Ok(None)`).
    fn load(
        &self,
        tenant: TenantId,
        store: StoreId,
    ) -> impl Future<Output = Result<Option<Versioned<ConfigTreeState>>, ConfigStoreError>> + Send;

    /// Persists a store's tree state, replacing the prior one **only if it is still at `expected`**
    /// ([ADR-0095](../../../docs/adr/0095-conditional-writes-for-collections.md)).
    ///
    /// `expected` is `None` for a store that has no row yet: the write must then *create* one, and
    /// is refused as [`UpdateOutcome::VersionMismatch`] if another publish created it first. That
    /// case is expressible here — unlike for the keyed upserts ADR-0095 defers — because "no row"
    /// and "a row at some version" are distinguishable states of one document.
    ///
    /// # Why this is the check that matters
    ///
    /// A handler also compares the caller's `If-Match` against the `ConfigVersionId` it loaded, and
    /// that comparison is what lets a refusal *name* the version an operator was editing. But it is
    /// checked against a read that may already be stale: two publishes can both load at `v1`, both
    /// find the caller's token matches, and both save. **A check against your own stale read is not
    /// a check.** This precondition is evaluated by the store at write time, and it is what actually
    /// prevents the interleave.
    ///
    /// # Errors
    ///
    /// [`ConfigStoreError`] if the store could not be written.
    fn save(
        &self,
        tenant: TenantId,
        store: StoreId,
        state: &ConfigTreeState,
        expected: Option<&Version>,
    ) -> impl Future<Output = Result<UpdateOutcome, ConfigStoreError>> + Send;

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

    /// Records a store's lightweight heartbeat ([ADR-0068](../../../docs/adr/0068-fleet-liveness.md)
    /// slice 2): advances `last_seen_at` to `seen_at`, and records `outbox_depth` when the store
    /// reported one, for a store that is up but not currently pulling config (a parked long-poll, or
    /// a quiet period between publishes). It leaves the recorded held version and last-config-pull
    /// instant untouched — a heartbeat is "I am here", not "I pulled". Unlike the config-pull capture
    /// this is the request's whole purpose, so the caller surfaces a failure rather than swallowing
    /// it.
    ///
    /// `outbox_depth` is how many events the store has committed and not yet published
    /// ([`EventStore::outbox_depth`](pos_ports::event_store::EventStore::outbox_depth)) — the store's
    /// own backlog, the opposite direction from the relay backlog the fleet row already carries.
    /// `None` means the store did not say (an older edge, or one whose log could not be read), which
    /// is not the same answer as zero and must leave whatever was last recorded alone.
    ///
    /// # Errors
    ///
    /// [`ConfigStoreError`] if the liveness row could not be written.
    fn record_store_heartbeat(
        &self,
        tenant: TenantId,
        store: StoreId,
        seen_at: Timestamp,
        outbox_depth: Option<u64>,
        lease_generation: Option<u64>,
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
