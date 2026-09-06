// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The fleet read model seam ([ADR-0068](../../../docs/adr/0068-fleet-liveness.md) slice 3).
//!
//! The console's operational answer to "is the fleet up, and is it in sync?" reads four facts about
//! each store together — its identity and status (the [registry](crate::registry)), its liveness
//! (`store_liveness`, captured on every config pull and heartbeat), its config drift (the version it
//! holds vs the one currently published), its relay backlog (unreported orders queued for its POS),
//! and its own publish backlog (events committed at the store and not yet shipped, as of its last
//! heartbeat). None of those is a new write; this seam only *reads* them, joined per tenant.
//!
//! Online/offline is deliberately **not** a stored fact — a store never announces it went quiet — so
//! it is derived at read time by the caller from `now − last_seen_at` against a freshness threshold
//! the HTTP layer owns. This seam therefore returns the raw `last_seen_at`, not a boolean, so the
//! threshold stays one decision in one place. A trait so it runs against an in-memory fake in tests
//! and a `store-postgres` join in the cloud (the impl lives in [`crate::persistence`], the SQL in
//! `store-postgres`).

use core::future::Future;

use crate::lease::StorePlacement;
use pos_proto::ids::{StoreId, TenantId};
use pos_proto::time::Timestamp;

use crate::registry::EntityStatus;

/// One store's fleet row: identity and status, liveness, config drift, and relay backlog. Every
/// liveness/backlog field is optional because a store may be registered but never yet seen,
/// un-configured (nothing published), or have an empty queue.
///
/// The config versions are kept as raw strings, not parsed [`ConfigVersionId`](pos_proto::ids::ConfigVersionId)s:
/// the *published* version is one the cloud minted (always well-formed), but the *held* version is
/// whatever the edge last reported, and a malformed report must not fail the whole fleet read — the
/// console shows it verbatim and flags the drift.
#[derive(Debug, Clone)]
pub struct FleetRow {
    /// The store id.
    pub store_id: StoreId,
    /// The human name.
    pub name: String,
    /// Active or archived (the registry status).
    pub status: EntityStatus,
    /// The store's most recent contact (a config pull or a heartbeat), or `None` if it has never
    /// checked in. The caller derives online/offline from this against its freshness threshold.
    pub last_seen_at: Option<Timestamp>,
    /// The store's most recent *config pull* specifically, or `None` — distinct from `last_seen_at`,
    /// which a bare heartbeat also advances.
    pub last_config_pull_at: Option<Timestamp>,
    /// The config version the store reported holding on its last pull, or `None`.
    pub config_version_held: Option<String>,
    /// The store's currently-published config version, or `None` if nothing has been published to it.
    pub config_version_published: Option<String>,
    /// How many orders are queued for the store's POS and not yet reported.
    pub relay_backlog: u64,
    /// When the oldest still-pending queued order arrived, or `None` if the queue is empty.
    pub relay_oldest_pending_at: Option<Timestamp>,
    /// The binary version the store last reported running ([ADR-0078](../../../docs/adr/0078-sync-and-ota-closure.md)),
    /// or `None` if it has never reported. A raw string for the same reason the held config version is:
    /// it is whatever the edge last said.
    pub installed_version: Option<String>,
    /// Whether the store's last post-install self-test passed, or `None` if it has never reported.
    pub self_test_ok: Option<bool>,
    /// When the store last reported an update outcome, or `None`.
    pub reported_at: Option<Timestamp>,
    /// How many events the store has committed and not yet published, as of its last heartbeat, or
    /// `None` if it has never reported one (an older edge, or one whose log could not be read).
    ///
    /// The store's *own* backlog, and the mirror image of [`relay_backlog`](Self::relay_backlog):
    /// that one counts orders the cloud is holding *for* the store, this one counts sales the store
    /// is holding *from* the cloud. `None` is deliberately not zero — "did not say" and "nothing
    /// pending" are different answers, and a console that showed both as `0` would report a silent
    /// store as caught up.
    pub outbox_depth: Option<u64>,
    /// When the store reported [`outbox_depth`](Self::outbox_depth), or `None`. A depth is only as
    /// current as the heartbeat that carried it, so the instant travels with it.
    pub outbox_reported_at: Option<Timestamp>,
    /// The lease generation the box last reported holding
    /// ([ADR-0108](../../../docs/adr/0108-the-lease-generation-is-authority.md)), or `None` if it
    /// has never said — an older edge, or one whose lease row could not be read.
    ///
    /// Read together with [`lease_generation_authoritative`](Self::lease_generation_authoritative):
    /// the *pair* is what makes a **split** legible. Either number alone leaves a replaced machine
    /// looking exactly like a quiet one.
    pub lease_generation_held: Option<u64>,
    /// When the store reported it, or `None`. Travels with the generation for the same reason the
    /// outbox instant does.
    pub lease_reported_at: Option<Timestamp>,
    /// The store's **authoritative** lease generation, or `None` if the cloud has never issued this
    /// store one — every store until an operator deliberately does, which reads as "no lease in
    /// force" and never as generation `0`.
    pub lease_generation_authoritative: Option<u64>,
    /// Where the machine holding that generation runs
    /// ([ADR-0110](../../../docs/adr/0110-edge-placement-is-a-deployment-axis.md)).
    ///
    /// Three-valued rather than `Option<EdgePlacement>`, because a fleet reader must tell a store
    /// nobody has bumped from a store whose stored token this binary cannot read — see
    /// [`StorePlacement`]. The alert engine reads this same row, and those two absences carry
    /// opposite severities.
    pub edge_placement: StorePlacement,
}

/// Reads the fleet read model, per tenant.
pub trait FleetStore {
    /// Every store's fleet row for a tenant, newest store first.
    ///
    /// # Errors
    ///
    /// [`FleetStoreError`] if the underlying store could not be read.
    fn list_fleet(
        &self,
        tenant: TenantId,
    ) -> impl Future<Output = Result<Vec<FleetRow>, FleetStoreError>> + Send;

    /// One store's fleet row within its tenant, or `None` if the tenant has no such store.
    ///
    /// # Errors
    ///
    /// [`FleetStoreError`] if the underlying store could not be read.
    fn store_detail(
        &self,
        tenant: TenantId,
        store: StoreId,
    ) -> impl Future<Output = Result<Option<FleetRow>, FleetStoreError>> + Send;
}

/// Records a store's OTA report onto the liveness read model ([ADR-0078](../../../docs/adr/0078-sync-and-ota-closure.md)):
/// the version it is now running and whether its self-test passed. A report is also a liveness contact,
/// so it advances the store's `last_seen_at`. The only *write* to the fleet read model besides the
/// config-pull/heartbeat liveness capture; a trait so it runs against a fake in tests and a
/// `store-postgres` upsert in the cloud.
pub trait OtaReportStore {
    /// Records that `store` (in `tenant`) is now running `installed` and whether its self-test
    /// passed, at `reported_at`.
    ///
    /// `self_test_passed` is `None` for a store that has never self-tested, which writes SQL `NULL`
    /// into a column that was always nullable — so [`FleetStore::self_test_ok`]'s `None` becomes
    /// reachable for the case the console already renders it as (ADR-0078 Amendment 1).
    ///
    /// # Errors
    ///
    /// [`FleetStoreError`] if the liveness row could not be written.
    fn record_report(
        &self,
        tenant: TenantId,
        store: StoreId,
        installed: &str,
        self_test_passed: Option<bool>,
        reported_at: Timestamp,
    ) -> impl Future<Output = Result<(), FleetStoreError>> + Send;
}

/// A failure of the fleet read model itself — one of the joined tables could not be read, a stored
/// value could not be decoded, or a report could not be written.
#[derive(Debug, thiserror::Error)]
#[error("the fleet store failed: {0}")]
pub struct FleetStoreError(String);

impl FleetStoreError {
    /// Wraps a message (for the server's log).
    #[must_use]
    pub fn new(message: impl Into<String>) -> Self {
        Self(message.into())
    }
}
