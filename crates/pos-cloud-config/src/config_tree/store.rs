// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The seam that persists a store's [`ConfigTree`](super::ConfigTree) state
//! ([ADR-0033](../../../../docs/adr/0033-config-tree.md)).
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

/// One bound print agent, as the store last reported it
/// ([ADR-0112](../../../docs/adr/0112-print-agents.md)).
///
/// Ids are kept as raw strings for the reason the config versions below are: the cloud did not mint
/// the paired device id — the store did — and one report the cloud cannot parse must not fail a
/// whole fleet read. The console shows what the store said.
///
/// Carries no personal data by construction: two opaque identifiers and a duration. Nothing a ticket
/// *says* helps diagnose a stalled agent, so nothing a ticket says is here.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
pub struct PrintAgentStanding {
    /// The terminal a console admin created and a manager bound at the till.
    pub agent_device_id: String,
    /// The paired device answering for it.
    pub paired_device_id: String,
    /// How long the oldest still-unacknowledged job has waited, or `None` when nothing is waiting.
    ///
    /// `None` is the healthy answer and is deliberately not zero: an agent with an empty queue and
    /// one whose ticket arrived this instant are different states, and only one is about to become
    /// an alert.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub oldest_unacknowledged_secs: Option<u64>,
}

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
    /// ([ADR-0095](../../../../docs/adr/0095-conditional-writes-for-collections.md)).
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
    /// ([ADR-0068](../../../../docs/adr/0068-fleet-liveness.md)): the contact instant `seen_at` and the
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

    /// Records a store's lightweight heartbeat ([ADR-0068](../../../../docs/adr/0068-fleet-liveness.md)
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
        print_agents: Option<Vec<PrintAgentStanding>>,
    ) -> impl Future<Output = Result<(), ConfigStoreError>> + Send;

    /// Records what a heartbeat said about the store's package
    /// ([ADR-0189](../../../../docs/adr/0189-a-store-runs-a-package-version-hq-pins.md) decision 6):
    /// the version it runs and the configuration it last refused for its package, replacing what was
    /// recorded. A heartbeat that said nothing about it is not passed here.
    ///
    /// The default records nothing, for a store that keeps no fleet row.
    ///
    /// # Errors
    ///
    /// [`ConfigStoreError`] if the liveness row could not be written.
    fn record_package_beat(
        &self,
        tenant: TenantId,
        store: StoreId,
        seen_at: Timestamp,
        beat: &PackageBeat,
    ) -> impl Future<Output = Result<(), ConfigStoreError>> + Send {
        let _ = (tenant, store, seen_at, beat);
        async { Ok(()) }
    }

    /// Records what a heartbeat said about how configuration reaches the store and how fast
    /// ([ADR-0195](../../../../docs/adr/0195-the-cloud-wakes-a-store-to-pull-its-config.md)), replacing
    /// what was recorded. A heartbeat that said nothing about it is not passed here.
    ///
    /// The default records nothing, for a store that keeps no fleet row.
    ///
    /// # Errors
    ///
    /// [`ConfigStoreError`] if the liveness row could not be written.
    fn record_config_wake_beat(
        &self,
        tenant: TenantId,
        store: StoreId,
        seen_at: Timestamp,
        beat: &ConfigWakeBeat,
    ) -> impl Future<Output = Result<(), ConfigStoreError>> + Send {
        let _ = (tenant, store, seen_at, beat);
        async { Ok(()) }
    }

    /// Records which package version each of the store's paired tills last read, as a heartbeat
    /// said (roadmap-v3 B7.3), replacing what was recorded. A heartbeat that said nothing about its
    /// tills is not passed here.
    ///
    /// The default records nothing, for a store that keeps no fleet row.
    ///
    /// # Errors
    ///
    /// [`ConfigStoreError`] if the liveness row could not be written.
    fn record_device_reads_beat(
        &self,
        tenant: TenantId,
        store: StoreId,
        seen_at: Timestamp,
        reads: &[DeviceReadBeat],
    ) -> impl Future<Output = Result<(), ConfigStoreError>> + Send {
        let _ = (tenant, store, seen_at, reads);
        async { Ok(()) }
    }
}

/// What one heartbeat said about one paired till (B7.3): the package version it last read from the
/// edge, and when. A device id names a device, never a person.
#[derive(Clone, Debug, PartialEq, Eq, serde::Deserialize, serde::Serialize, utoipa::ToSchema)]
pub struct DeviceReadBeat {
    /// The till, a ULID.
    pub device_id: String,
    /// The package version it was served, or `null` for its release's default package.
    #[schema(required = true)]
    pub package_version_id: Option<String>,
    /// When it read it, in Unix milliseconds on the store's clock.
    pub read_at_ms: i64,
}

impl DeviceReadBeat {
    /// The most tills one heartbeat reports, as the edge holds at most that many.
    pub const DEVICES_MAX: usize = 64;

    /// The longest id a beat keeps, in characters.
    pub const TEXT_MAX: usize = 64;

    /// Whether `reads` is a report an edge could have sent: at most [`Self::DEVICES_MAX`] tills,
    /// each named once, with ids no longer than an id.
    #[must_use]
    pub fn are_bounded(reads: &[Self]) -> bool {
        let short = |text: &str| text.chars().count() <= Self::TEXT_MAX;
        let mut ids: Vec<&str> = reads.iter().map(|read| read.device_id.as_str()).collect();
        ids.sort_unstable();
        ids.dedup();
        reads.len() <= Self::DEVICES_MAX
            && ids.len() == reads.len()
            && reads.iter().all(|read| {
                short(&read.device_id) && read.package_version_id.as_deref().is_none_or(short)
            })
    }
}

/// What one heartbeat said about config delivery (ADR-0195): whether the store hears the cloud's
/// wake or runs on the poll alone, and the time from wake to apply over its latest wakes.
#[derive(Clone, Debug, PartialEq, Eq, serde::Deserialize, serde::Serialize, utoipa::ToSchema)]
pub struct ConfigWakeBeat {
    /// `CONFIG_DELIVERY_WAKE`, or `CONFIG_DELIVERY_POLL` for a store on the poll alone.
    pub delivery: String,
    /// How many wake-to-apply times the figures are drawn from; the edge keeps its latest 128.
    pub samples: u32,
    /// Their 95th percentile, in milliseconds; absent with no samples.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schema(nullable = false)]
    pub p95_ms: Option<u64>,
    /// The longest of them, in milliseconds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schema(nullable = false)]
    pub max_ms: Option<u64>,
}

impl ConfigWakeBeat {
    /// The longest delivery name a beat keeps, in characters.
    pub const TEXT_MAX: usize = 64;

    /// The most samples a beat may say it drew from.
    pub const SAMPLES_MAX: u32 = 1024;

    /// Whether the beat is one an edge could have sent.
    #[must_use]
    pub fn is_bounded(&self) -> bool {
        self.delivery.chars().count() <= Self::TEXT_MAX && self.samples <= Self::SAMPLES_MAX
    }
}

/// What one heartbeat said about the store's package.
#[derive(
    Clone, Debug, Default, PartialEq, Eq, serde::Deserialize, serde::Serialize, utoipa::ToSchema,
)]
pub struct PackageBeat {
    /// The package version the store runs, or absent for its release's default package.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schema(nullable = false)]
    pub package_version_id: Option<String>,
    /// The configuration the store last refused for its package, while the latest one was.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schema(nullable = false)]
    pub refused: Option<RefusedBeat>,
}

/// A configuration a store refused for its package; it kept the one before.
#[derive(Clone, Debug, PartialEq, Eq, serde::Deserialize, serde::Serialize, utoipa::ToSchema)]
pub struct RefusedBeat {
    /// The configuration version refused.
    pub config_version_id: String,
    /// The package version it carried, when the store could read it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schema(nullable = false)]
    pub package_version_id: Option<String>,
    /// `PACKAGE_MALFORMED`, `PACKAGE_INVALID` or `EDGE_TOO_OLD`.
    pub reason: String,
}

impl PackageBeat {
    /// The longest id or reason a beat keeps, in characters; longer text is not one this cloud wrote.
    pub const TEXT_MAX: usize = 64;

    /// Whether every text the beat carries is within [`Self::TEXT_MAX`].
    #[must_use]
    pub fn is_bounded(&self) -> bool {
        let fits = |text: &str| text.chars().count() <= Self::TEXT_MAX;
        self.package_version_id.as_deref().is_none_or(fits)
            && self.refused.as_ref().is_none_or(|refused| {
                fits(&refused.config_version_id)
                    && fits(&refused.reason)
                    && refused.package_version_id.as_deref().is_none_or(fits)
            })
    }
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
