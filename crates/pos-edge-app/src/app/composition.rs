// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The edge's own wiring: composing it, the session it trades on and swaps when a configuration
//! arrives, the lease standing that stops a superseded box opening anything new, the status bar's
//! sync and clock cells, the fan-out, and the system facts it records about devices.

use std::sync::{Arc, Mutex, RwLock};

use pos_core::business_date::derive_business_date;
use pos_ports::PortError;
use pos_ports::event_store::EventStore;
use pos_proto::ClockSource;
use pos_proto::envelope::{EventEnvelope, EventPayload, EventTypeRef, RawPayload};
use pos_proto::events::{
    DeviceActivationCompleted, DeviceAdmissionGranted, DeviceAdmissionRevoked,
};
use pos_proto::ids::{DeviceId, OrderLineId, StoreId};

use super::{AppError, Edge, EdgeSession, Projection, StoreIdentity};
use crate::auth::Lockout;
use crate::clock::SystemClock;
use crate::fanout::{Fanout, ServerMessage};
use crate::idgen::EdgeIdGenerator;
use crate::lease_state::CurrentStanding;
use crate::line_notes::LineNotes;
use crate::receipt::ReceiptAuthority;
use crate::sntp::ClockStatus;
use crate::sync_status::{OUTBOX_PLANNED_DEPTH, OutboxLevel, SyncReport, SyncStatus};

impl<S> Edge<S> {
    /// A snapshot of the synced session — the menu catalog and tax table an inbound `OrderIn`
    /// reprices from. A cheap `Arc` clone, so a caller reads a coherent session for the whole of its
    /// command even if the config-pull loop swaps in a new one mid-flight.
    #[must_use]
    #[expect(
        clippy::missing_panics_doc,
        reason = "the only panic is a poisoned session lock, unreachable here: the critical section \
                  just clones an Arc and cannot itself panic"
    )]
    pub fn session(&self) -> Arc<EdgeSession> {
        self.session_cell
            .read()
            .expect("session lock is not poisoned")
            .clone()
    }

    /// Swaps in a rebuilt session — the config-pull loop's apply step
    /// ([ADR-0039](../../../../docs/adr/0039-config-delivery.md)). Commands already in flight keep the
    /// [`Arc`] snapshot they took; the next command sees the new session. Cloud-owned configuration
    /// (ADR-0004): the store never edits this, it only receives it.
    #[expect(
        clippy::missing_panics_doc,
        reason = "the only panic is a poisoned session lock, unreachable here: the critical section \
                  just replaces an Arc and cannot itself panic"
    )]
    pub fn apply_session(&self, session: EdgeSession) {
        *self
            .session_cell
            .write()
            .expect("session lock is not poisoned") = Arc::new(session);
    }

    /// The lease standing this box is acting on
    /// ([ADR-0123](../../../../docs/adr/0123-a-superseded-box-opens-nothing-new.md)).
    ///
    /// Lent as the shared cell rather than copied out, because the config-pull loop writes it and
    /// the request path reads it, and a second copy is a second answer.
    #[must_use]
    pub const fn lease(&self) -> &Arc<CurrentStanding> {
        &self.standing
    }

    /// What this box last measured of its own clock against a time server (roadmap-v3 PF4).
    ///
    /// Lent rather than copied out for the reason [`Self::lease`] is: the probe loop writes it and
    /// the status route reads it.
    #[must_use]
    pub const fn clock_status(&self) -> &ClockStatus {
        &self.clock_status
    }

    /// Refuses a command that would **open** something new when a replacement machine has taken this
    /// store ([ADR-0123](../../../../docs/adr/0123-a-superseded-box-opens-nothing-new.md)).
    ///
    /// Called by the four commands that mint an [`OrderId`] or a [`ShiftId`] from nothing, and by
    /// no others — `open_shift`, `seat_table`, `open_inbound_order` and `open_counter_order`, which
    /// are exactly the sites in this file that call `next_order_id` or construct a `ShiftId`. Everything already open runs to settlement on a superseded box — a seated table takes
    /// its second round, gets its bill and is paid — because the order lives in *this* box's log and
    /// there is nowhere else for those guests to be served. What the box cannot do is start
    /// anything: the floor drains and does not refill.
    ///
    /// # Errors
    ///
    /// [`AppError::Superseded`] when, and only when, the standing is
    /// [`Superseded`](pos_core::lease::LeaseStanding::Superseded).
    pub(in crate::app) fn refuse_if_superseded(&self) -> Result<(), AppError> {
        if self.standing.opens_new_work() {
            Ok(())
        } else {
            Err(AppError::Superseded)
        }
    }

    /// The lines currently marked prepared (`kitchen.ticket.bumped`), for a KDS reading the current
    /// state on (re)connect.
    ///
    /// # Panics
    ///
    /// If the projection lock is poisoned — unreachable, as its critical section only reads the set.
    #[must_use]
    pub fn bumped_line_ids(&self) -> Vec<OrderLineId> {
        self.projection
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .bumped_line_ids()
    }
}

impl<S: EventStore> Edge<S> {
    /// Composes an edge over `store` for a given identity and session, allocating receipt numbers
    /// from `receipts`.
    ///
    /// The real binary passes the SQLite store itself (its writer thread is the gapless authority,
    /// ADR-0025); the example and tests pass an [`InMemoryReceipts`](crate::receipt::InMemoryReceipts).
    ///
    /// # Errors
    ///
    /// [`getrandom::Error`] if the OS entropy source needed to seed the id generator is unavailable.
    pub fn new(
        store: S,
        identity: StoreIdentity,
        session: EdgeSession,
        receipts: Arc<dyn ReceiptAuthority>,
    ) -> Result<Self, getrandom::Error> {
        Ok(Self {
            store,
            identity,
            session_cell: RwLock::new(Arc::new(session)),
            clock: SystemClock,
            ids: Mutex::new(EdgeIdGenerator::new(SystemClock)?),
            fanout: Fanout::new(),
            projection: Mutex::new(Projection::default()),
            receipts,
            standing: Arc::new(CurrentStanding::new()),
            sync: SyncStatus::new(),
            clock_status: ClockStatus::new(),
            notes: Mutex::new(LineNotes::default()),
            pin_lockout: Arc::new(Lockout::new()),
        })
    }

    /// The PIN lockout sign-ins and approvals share ([`Lockout`], ADR-0030).
    #[must_use]
    pub fn pin_lockout(&self) -> Arc<Lockout> {
        Arc::clone(&self.pin_lockout)
    }

    /// The cloud-link and outbox-depth cell the status bar reads
    /// ([ADR-0137](../../../../docs/adr/0137-a-deep-outbox-warns-and-never-refuses.md)).
    #[must_use]
    pub const fn sync(&self) -> &SyncStatus {
        &self.sync
    }

    /// What the status bar shows: how many events wait for the cloud, how worried to be about it,
    /// and whether the cloud was reachable on the drain's last pass.
    ///
    /// The depth is reused for [`DEPTH_TTL`](crate::sync_status::DEPTH_TTL) so every device polling
    /// at once costs the store one count.
    ///
    /// # Errors
    ///
    /// [`AppError::Port`] if the depth had to be measured and the store could not be read.
    pub async fn sync_report(&self) -> Result<SyncReport, AppError> {
        let now = tokio::time::Instant::now();
        let depth = if let Some(depth) = self.sync.cached_depth(now) {
            depth
        } else {
            let depth = self.store.outbox_depth(self.identity.store_id).await?;
            self.sync.record_depth(depth, now);
            depth
        };
        Ok(SyncReport {
            outbox_depth: depth,
            outbox_level: OutboxLevel::of(depth, OUTBOX_PLANNED_DEPTH),
            cloud_link: self.sync.cloud_link(),
            last_sync_time: self.sync.last_sync_time(),
        })
    }

    /// The fan-out devices subscribe to.
    #[must_use]
    pub fn fanout(&self) -> &Fanout {
        &self.fanout
    }

    /// Tells every connected device that a new store configuration is live
    /// ([ADR-0160](../../../../docs/adr/0160-everything-a-store-runs-differently-is-published-configuration.md)
    /// item 7), so an open till reloads its floor, menu, layout, locale and reason codes without a
    /// new sign-in. Numbered on the fan-out like any frame, so a till that reconnects is replayed it.
    ///
    /// `changed_nodes` names the top-level nodes that changed, where the caller knows them, so a
    /// till re-reads only what derives from them; `None` has every till re-read everything.
    pub fn announce_config(&self, config_version_id: &str, changed_nodes: Option<Vec<String>>) {
        self.fanout.publish(&ServerMessage::ConfigApplied {
            config_version_id: config_version_id.to_owned(),
            changed_nodes,
        });
    }

    /// The store this edge is, so a driving-port caller (the inbound `OrderIn`) can bind an order to
    /// it and refuse one addressed to another store.
    #[must_use]
    pub fn store_id(&self) -> StoreId {
        self.identity.store_id
    }

    /// Makes sure this box never issues a receipt number at or below `floor`, the highest the cloud
    /// has seen from this store
    /// ([ADR-0149](../../../../docs/adr/0149-a-replacement-box-numbers-above-what-the-cloud-has-seen.md)).
    /// A counter already above it is left alone.
    ///
    /// # Errors
    ///
    /// [`PortError`] if the receipt authority cannot be written.
    pub async fn raise_receipt_floor(&self, floor: u64) -> Result<(), PortError> {
        self.receipts.raise_floor(self.store_id(), floor).await
    }

    /// The event log this edge owns, lent read-only.
    ///
    /// The one caller is the outbox drain
    /// (`pos_edge::event_publish`, [ADR-0087](../../../../docs/adr/0087-edge-relay-and-event-publish.md)),
    /// which needs an [`EventStore`] handle to read `outbox_batch` and acknowledge what the cloud
    /// accepted. Lending the store beats threading a second handle through `serve()` — the `Edge`
    /// already owns it, and a borrow cannot append behind the application layer's back: every write
    /// still goes through `Edge`'s own load → decide → apply path.
    #[must_use]
    pub const fn store(&self) -> &S {
        &self.store
    }

    /// Records that this box completed device activation and may now trade
    /// (`device.activation.completed`, [ADR-0050](../../../../docs/adr/0050-activation-code-exchange.md)).
    ///
    /// A system event, not a domain decision: there is no signed-in employee at first boot, so the
    /// envelope carries none, and the box's own new identity is both the reporting and the activated
    /// device. The caller stores the credential in the [`KeyVault`](pos_ports::KeyVault) *before*
    /// calling this — the vault is what the boot gate reads, so it, not this event, is the source of
    /// truth for "activated"; this event is the notification that flows to the cloud.
    ///
    /// # Errors
    ///
    /// [`AppError`] if the business date cannot be derived, the event cannot be encoded, or the store
    /// cannot be written.
    pub async fn record_activation(&self, activated_device_id: DeviceId) -> Result<(), AppError> {
        self.record_device_fact(
            activated_device_id,
            &DeviceActivationCompleted {
                activated_device_id,
            },
        )
        .await
    }

    /// Records that the store admitted a device by pairing it (`device.admission.granted`,
    /// [ADR-0118](../../../../docs/adr/0118-one-credential-per-box-and-the-cloud-learns.md) §4).
    ///
    /// `admitted_by_device_id` is the paired device whose signed-in manager minted the code, and
    /// `None` for the code a boot announces — nothing authorised that one, because nothing could.
    ///
    /// # No employee reaches this event, deliberately
    ///
    /// The signature cannot carry one, which is the point: an employee identity here would be a
    /// durable, central, cross-border, attributable record of managerial activity, and the purpose
    /// this event serves is served in full by device ids (ADR-0118 §5 and its *Compliance posture*).
    /// Who admitted a device is recorded on the store's own disk, as
    /// [`PairedDevice::admitted_by`](pos_ports::device_registry::PairedDevice::admitted_by).
    ///
    /// # Errors
    ///
    /// [`AppError`] if the business date cannot be derived, the event cannot be encoded, or the store
    /// cannot be written. Callers on the admission path **log and continue**: a device the store has
    /// already admitted must not be un-admitted because an event could not be written.
    pub async fn record_device_admitted(
        &self,
        admitted_device_id: DeviceId,
        admitted_by_device_id: Option<DeviceId>,
    ) -> Result<(), AppError> {
        self.record_device_fact(
            admitted_device_id,
            &DeviceAdmissionGranted {
                admitted_device_id,
                admitted_by_device_id,
            },
        )
        .await
    }

    /// Records that the store retired a device's admission (`device.admission.revoked`,
    /// [ADR-0118](../../../../docs/adr/0118-one-credential-per-box-and-the-cloud-learns.md) §4).
    ///
    /// One call per device, including for the break-glass that retires every device at once, so a
    /// reader never has to interpret an absent field as *all of them*.
    ///
    /// # Errors
    ///
    /// [`AppError`] as [`Self::record_device_admitted`], and treated the same way by its caller: the
    /// device is already revoked locally when this runs, and a revocation must not be undone because
    /// the cloud could not be told about it.
    pub async fn record_device_revoked(&self, revoked_device_id: DeviceId) -> Result<(), AppError> {
        self.record_device_fact(
            revoked_device_id,
            &DeviceAdmissionRevoked { revoked_device_id },
        )
        .await
    }

    /// Appends one **system** fact about a device and publishes it to the connected screens.
    ///
    /// Shared by activation, admission and revocation because they share everything about them that
    /// is easy to get wrong, and because a fourth copy of this envelope was the alternative:
    ///
    /// * **no employee.** There is none at first boot, and an admission records none by decision
    ///   (ADR-0118 §5). An `employee_id` that crept onto one of these would be a compliance change
    ///   disguised as a refactor, so no caller here is able to supply one.
    /// * **no shift.** None of the three is a trading act and none of them opens or belongs to one.
    /// * **the subject device reports.** A system event has no separate reporter, so the device the
    ///   fact is *about* is the device on the envelope — which is what `record_activation` has always
    ///   done, since at first boot the box's new identity is all there is.
    async fn record_device_fact<P: EventPayload>(
        &self,
        device_id: DeviceId,
        payload: &P,
    ) -> Result<(), AppError> {
        let now = self.clock.now();
        let session = self.session();
        let business_date = derive_business_date(now, &session.timezone, session.cutoff)
            .map_err(|_ignored| AppError::Clock)?;
        let data = RawPayload::encode(payload).map_err(AppError::Encode)?;
        let envelope = EventEnvelope {
            event_id: pos_proto::ids::EventId::new(self.next_ulid()),
            event_type: EventTypeRef::from_known(P::EVENT_TYPE),
            event_time: now,
            business_date,
            schema_version: P::SCHEMA_VERSION,
            tenant_id: self.identity.tenant_id,
            brand_id: self.identity.brand_id,
            store_id: self.identity.store_id,
            device_id,
            employee_id: None,
            shift_id: None,
            // Unstamped here. The chain is assigned at append, inside the transaction,
            // because that is the only place the previous head is known (ADR-0131).
            chain: None,
            data,
        };
        let published = serde_json::to_value(payload).unwrap_or(serde_json::Value::Null);
        let message = ServerMessage::Event {
            event_type: EventTypeRef::from_known(P::EVENT_TYPE).as_str().to_owned(),
            payload: published,
        };
        self.append_and_publish(vec![envelope], vec![message]).await
    }
}
