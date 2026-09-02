// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! One store implementing both `EventStore` and `ConfigStore`, and therefore one transaction.
//!
//! That is not a convenience — it is the arrangement
//! [ADR-0026](../../../docs/adr/0026-port-shapes.md) §2 exists to force. `Transactional` is a
//! supertrait of both ports, so an adapter implementing both has exactly one `Tx` type, and
//! "the outbox row commits with the state change" becomes the only thing that type-checks.
//! `store-sqlite` will be shaped the same way.
//!
//! # Where the uncommitted writes live
//!
//! In the transaction handle, not in the store. That is what makes power loss a one-liner for the
//! harness and a real guarantee for the suite: a dropped [`FakeTx`] takes its pending writes with
//! it, exactly as an unflushed SQLite transaction does.
//!
//! # The outbox position is assigned at commit
//!
//! Deliberately, and the contract case checks it by committing a transaction whose identifiers sort
//! *below* an earlier one's. A fake that assigned positions at append time, or ordered by
//! `event_id`, would fail that case — which is the case existing for the right reason.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use pos_ports::config_store::{ConfigSnapshot, ConfigStore, ConfigUpdate};
use pos_ports::device_registry::{DeviceRegistry, DeviceSession, PairedDevice, TokenDigest};
use pos_ports::event_store::{AppendOutcome, EventQuery, EventStore, OutboxPosition, OutboxRecord};
use pos_ports::intake_ledger::{IntakeLedger, IntakeRecord};
use pos_ports::{PortError, PortName, Transactional, TxContext};
use pos_proto::envelope::{EventEnvelope, RawPayload};
use pos_proto::ids::{ConfigVersionId, DeviceId, EventId, StoreId};
use pos_proto::time::Timestamp;

use crate::infra::FakeDeviceRegistry;
use crate::lock;

/// How many undelivered events a fake store holds before pushing back.
///
/// A real number rather than unbounded, because back-pressure is not testable against a queue that
/// cannot fill. High enough that no ordinary case reaches it.
pub const OUTBOX_CAPACITY: usize = 10_000;

/// Everything the store has committed.
#[derive(Debug, Default)]
struct StoreState {
    /// Per store, ordered by identifier — which is what `read` promises.
    events: BTreeMap<StoreId, BTreeMap<EventId, EventEnvelope<RawPayload>>>,
    /// Per store, in commit order.
    outbox: BTreeMap<StoreId, Vec<OutboxRecord>>,
    /// The next position to assign, global rather than per store: positions only have to be
    /// monotone within a store, and a single counter satisfies that while being harder to get
    /// subtly wrong.
    ///
    /// **Starts at one, not zero.** [`OutboxPosition::START`] is zero and means "before every
    /// event", and a reader asks for everything after it — so an event assigned position zero is
    /// invisible to the first poll. The contract suite caught exactly this, which is the case
    /// earning its place: a real adapter using a zero-based row number has the same bug, and its
    /// symptom is one lost event per store, once, at the very beginning.
    next_position: u64,
    /// The version each store is running.
    current: BTreeMap<StoreId, ConfigSnapshot>,
    /// The last version that applied, which diverges from `current` only after a refusal.
    last_known_good: BTreeMap<StoreId, ConfigSnapshot>,
    /// The inbound-order idempotency ledger, keyed by `(store, sales_channel, external_reference)`
    /// (ADR-0064). Written in the order's own transaction, so a committed record always has its
    /// order.
    intake: BTreeMap<(StoreId, String, String), IntakeRecord>,
}

/// An in-memory `EventStore`, `ConfigStore`, `IntakeLedger` and `DeviceRegistry`.
///
/// The device registry is a delegated [`FakeDeviceRegistry`] rather than more fields on
/// [`StoreState`], because the real `store-sqlite` adapter keeps it in the same database but shares
/// no transaction with the event log ([ADR-0091](../../../docs/adr/0091-durable-edge-auth-state.md)
/// explains why it is not `Transactional`). Composing the two fakes the same way keeps the
/// separation visible, and means the registry's thirteen contract cases exercise the same code
/// whichever fake a suite is handed.
#[derive(Debug, Clone, Default)]
pub struct FakeStore {
    state: Arc<Mutex<StoreState>>,
    devices: FakeDeviceRegistry,
}

impl FakeStore {
    /// An empty store.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Another handle onto the same committed state.
    ///
    /// What the harness returns from `lose_power`. It commits nothing and flushes nothing, so
    /// whatever an open transaction was holding is gone with it — which is the behaviour under test.
    #[must_use]
    pub fn reopen(&self) -> Self {
        Self {
            state: Arc::clone(&self.state),
            // The registry is in the same database, so a reopen sees the same rows — which is what
            // makes "a restart no longer unpairs the store" testable through this fake at all.
            devices: self.devices.clone(),
        }
    }
}

/// Writes waiting for a commit.
///
/// Not `Clone`: two handles onto one transaction would let a caller commit it twice, and the port's
/// `commit(self)` exists precisely to make that impossible.
#[derive(Debug)]
pub struct FakeTx {
    state: Arc<Mutex<StoreState>>,
    events: Vec<EventEnvelope<RawPayload>>,
    config: Option<ConfigUpdate>,
    /// The inbound-order idempotency row to write with the order (ADR-0064), if any:
    /// `(store, sales_channel, external_reference, record)`.
    intake: Option<(StoreId, String, String, IntakeRecord)>,
}

impl TxContext for FakeTx {
    async fn commit(self) -> Result<(), PortError> {
        let mut state = lock(&self.state);

        // Check the intake key BEFORE mutating anything, so a conflict leaves the whole transaction
        // with no effect — the same atomicity the real store gets from rolling back on the plain
        // insert's constraint violation (ADR-0064). A committed intake row therefore always has its
        // order, and a second order on the same key never lands.
        if let Some((store_id, sales_channel, external_reference, _)) = &self.intake
            && state.intake.contains_key(&(
                *store_id,
                sales_channel.clone(),
                external_reference.clone(),
            ))
        {
            return Err(PortError::already_exists(
                PortName::IntakeLedger,
                "an order already exists for this reference",
            ));
        }

        for envelope in self.events {
            let store_id = envelope.store_id;
            let stored = state.events.entry(store_id).or_default();
            if stored.contains_key(&envelope.event_id) {
                // Idempotent by identifier, and the *stored* copy wins. Not compared: a byte
                // difference at the same identifier is a sender bug a store cannot fix, and silently
                // preferring the newer one would make the log depend on delivery order.
                continue;
            }
            stored.insert(envelope.event_id, envelope.clone());

            let outbox = state.outbox.entry(store_id).or_default();
            if outbox.len() >= OUTBOX_CAPACITY {
                return Err(PortError::resource_exhausted(
                    PortName::EventStore,
                    "the outbox is at capacity",
                ));
            }
            state.next_position = state.next_position.saturating_add(1);
            let position = OutboxPosition::new(state.next_position);
            state
                .outbox
                .entry(store_id)
                .or_default()
                .push(OutboxRecord { position, envelope });
        }

        if let Some(update) = self.config {
            let snapshot = match update {
                ConfigUpdate::Snapshot(snapshot) => snapshot,
                ConfigUpdate::Delta(delta) => {
                    // A delta's target document is its patch: the fake does not merge, because the
                    // patch format is P7's decision and inventing one here would make the fake
                    // disagree with the real store in a way no case could catch.
                    ConfigSnapshot {
                        config_version_id: delta.to_config_version_id,
                        store_id: delta.store_id,
                        document: delta.patch,
                    }
                }
            };
            state.current.insert(snapshot.store_id, snapshot.clone());
            state.last_known_good.insert(snapshot.store_id, snapshot);
        }

        if let Some((store_id, sales_channel, external_reference, record)) = self.intake {
            state
                .intake
                .insert((store_id, sales_channel, external_reference), record);
        }

        Ok(())
    }

    async fn rollback(self) -> Result<(), PortError> {
        // Nothing to undo: pending writes live in `self`, so dropping it is the rollback. Stated
        // rather than left implicit, because "rollback is a no-op" reads like a bug until you know
        // where the writes were.
        Ok(())
    }
}

impl Transactional for FakeStore {
    type Tx = FakeTx;

    async fn begin(&self) -> Result<Self::Tx, PortError> {
        Ok(FakeTx {
            state: Arc::clone(&self.state),
            events: Vec::new(),
            config: None,
            intake: None,
        })
    }
}

impl EventStore for FakeStore {
    async fn append(
        &self,
        tx: &mut FakeTx,
        events: &[EventEnvelope<RawPayload>],
    ) -> Result<AppendOutcome, PortError> {
        let mut stores = events.iter().map(|envelope| envelope.store_id);
        if let Some(first) = stores.next()
            && !stores.all(|store_id| store_id == first)
        {
            return Err(PortError::invalid_argument(
                PortName::EventStore,
                "a batch must name one store",
            ));
        }

        let state = lock(&self.state);
        let mut outcome = AppendOutcome::default();
        for envelope in events {
            let already_committed = state
                .events
                .get(&envelope.store_id)
                .is_some_and(|stored| stored.contains_key(&envelope.event_id));
            let already_pending = tx
                .events
                .iter()
                .any(|pending| pending.event_id == envelope.event_id);
            if already_committed || already_pending {
                outcome.duplicates = outcome.duplicates.saturating_add(1);
            } else {
                outcome.appended = outcome.appended.saturating_add(1);
                tx.events.push(envelope.clone());
            }
        }
        Ok(outcome)
    }

    async fn read(&self, query: &EventQuery) -> Result<Vec<EventEnvelope<RawPayload>>, PortError> {
        let state = lock(&self.state);
        let Some(stored) = state.events.get(&query.store_id) else {
            return Ok(Vec::new());
        };
        let limit = usize::try_from(query.limit.get()).unwrap_or(usize::MAX);
        // Exclusive lower bound, which is what makes paging safe to resume: an inclusive one would
        // return the last event of every page twice.
        let lower = match query.after {
            Some(after) => std::ops::Bound::Excluded(after),
            None => std::ops::Bound::Unbounded,
        };
        Ok(stored
            .range((lower, std::ops::Bound::Unbounded))
            .take(limit)
            .map(|(_, event)| event.clone())
            .collect())
    }

    async fn contains(&self, store_id: StoreId, event_id: EventId) -> Result<bool, PortError> {
        let state = lock(&self.state);
        Ok(state
            .events
            .get(&store_id)
            .is_some_and(|stored| stored.contains_key(&event_id)))
    }

    async fn outbox_batch(
        &self,
        store_id: StoreId,
        after: OutboxPosition,
        limit: core::num::NonZeroU32,
    ) -> Result<Vec<OutboxRecord>, PortError> {
        let state = lock(&self.state);
        let limit = usize::try_from(limit.get()).unwrap_or(usize::MAX);
        Ok(state
            .outbox
            .get(&store_id)
            .into_iter()
            .flatten()
            .filter(|record| record.position > after)
            .take(limit)
            .cloned()
            .collect())
    }

    async fn acknowledge_outbox(
        &self,
        store_id: StoreId,
        through: OutboxPosition,
    ) -> Result<u64, PortError> {
        let mut state = lock(&self.state);
        let Some(outbox) = state.outbox.get_mut(&store_id) else {
            return Ok(0);
        };
        let before = outbox.len();
        // A high-water mark. Acknowledging the same position twice removes nothing extra, and
        // acknowledging backwards removes nothing at all — both properties the contract case checks,
        // and both of which fall out of retaining rather than counting.
        outbox.retain(|record| record.position > through);
        Ok(u64::try_from(before.saturating_sub(outbox.len())).unwrap_or(u64::MAX))
    }

    async fn outbox_depth(&self, store_id: StoreId) -> Result<u64, PortError> {
        let state = lock(&self.state);
        let depth = state.outbox.get(&store_id).map_or(0, Vec::len);
        Ok(u64::try_from(depth).unwrap_or(u64::MAX))
    }
}

impl ConfigStore for FakeStore {
    async fn current(&self, store_id: StoreId) -> Result<Option<ConfigSnapshot>, PortError> {
        let state = lock(&self.state);
        Ok(state.current.get(&store_id).cloned())
    }

    async fn last_known_good(
        &self,
        store_id: StoreId,
    ) -> Result<Option<ConfigSnapshot>, PortError> {
        let state = lock(&self.state);
        Ok(state.last_known_good.get(&store_id).cloned())
    }

    async fn apply(
        &self,
        tx: &mut FakeTx,
        update: &ConfigUpdate,
    ) -> Result<ConfigVersionId, PortError> {
        let state = lock(&self.state);
        let reached = match update {
            ConfigUpdate::Snapshot(snapshot) => snapshot.config_version_id,
            ConfigUpdate::Delta(delta) => {
                let held = state
                    .current
                    .get(&delta.store_id)
                    .map(|snapshot| snapshot.config_version_id);
                if held != Some(delta.from_config_version_id) {
                    return Err(PortError::failed_precondition(
                        PortName::ConfigStore,
                        "the delta does not apply from the version this store holds",
                    ));
                }
                delta.to_config_version_id
            }
        };
        tx.config = Some(update.clone());
        Ok(reached)
    }
}

impl IntakeLedger for FakeStore {
    async fn record(
        &self,
        tx: &mut FakeTx,
        store_id: StoreId,
        sales_channel: &str,
        external_reference: &str,
        record: &IntakeRecord,
    ) -> Result<(), PortError> {
        tx.intake = Some((
            store_id,
            sales_channel.to_owned(),
            external_reference.to_owned(),
            record.clone(),
        ));
        Ok(())
    }

    async fn look_up(
        &self,
        store_id: StoreId,
        sales_channel: &str,
        external_reference: &str,
    ) -> Result<Option<IntakeRecord>, PortError> {
        let state = lock(&self.state);
        Ok(state
            .intake
            .get(&(
                store_id,
                sales_channel.to_owned(),
                external_reference.to_owned(),
            ))
            .cloned())
    }
}

impl DeviceRegistry for FakeStore {
    async fn record_pairing(&self, device: PairedDevice) -> Result<(), PortError> {
        self.devices.record_pairing(device).await
    }

    async fn device_for_token(&self, digest: TokenDigest) -> Result<Option<DeviceId>, PortError> {
        self.devices.device_for_token(digest).await
    }

    async fn paired_devices(&self) -> Result<Vec<PairedDevice>, PortError> {
        self.devices.paired_devices().await
    }

    async fn revoke_device(&self, device_id: DeviceId) -> Result<(), PortError> {
        self.devices.revoke_device(device_id).await
    }

    async fn revoke_all_devices(&self) -> Result<(), PortError> {
        self.devices.revoke_all_devices().await
    }

    async fn record_sign_in(&self, session: DeviceSession) -> Result<(), PortError> {
        self.devices.record_sign_in(session).await
    }

    async fn sign_in_for(&self, device_id: DeviceId) -> Result<Option<DeviceSession>, PortError> {
        self.devices.sign_in_for(device_id).await
    }

    async fn sign_ins(&self) -> Result<Vec<DeviceSession>, PortError> {
        self.devices.sign_ins().await
    }

    async fn touch_session(&self, device_id: DeviceId, now: Timestamp) -> Result<(), PortError> {
        self.devices.touch_session(device_id, now).await
    }

    async fn clear_sign_in(&self, device_id: DeviceId) -> Result<(), PortError> {
        self.devices.clear_sign_in(device_id).await
    }
}
