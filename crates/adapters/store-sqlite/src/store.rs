// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! [`SqliteStore`]: `open`, the writer-thread bridge, and the three port implementations.

use core::num::NonZeroU32;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use rusqlite::Connection;
use tokio::sync::{mpsc, oneshot};

use pos_ports::config_store::{ConfigSnapshot, ConfigStore, ConfigUpdate};
use pos_ports::device_registry::{DeviceRegistry, DeviceSession, PairedDevice, TokenDigest};
use pos_ports::event_store::{AppendOutcome, EventQuery, EventStore, OutboxPosition, OutboxRecord};
use pos_ports::intake_ledger::{IntakeLedger, IntakeRecord};
use pos_ports::subject_store::{SubjectRecord, SubjectStore};
use pos_ports::{PortError, PortName, Transactional};
use pos_proto::envelope::{EventEnvelope, RawPayload};
use pos_proto::ids::{BillId, ConfigVersionId, DeviceId, EventId, OrderId, StoreId, SubjectId};
use pos_proto::time::{BusinessDate, Timestamp};
use pos_proto::ulid::Ulid;

use crate::migrations;
use crate::tx::SqliteTx;
use crate::writer::{
    self, Command, DeviceSessionRow, IntakeWrite, PairedDeviceRow, RegistryCommand, SelfTestRow,
    SubjectWrite,
};

/// How many commands may queue for the writer thread before senders wait — back-pressure, so a
/// stalled writer cannot grow the queue without bound.
const COMMAND_CHANNEL_CAPACITY: usize = 1_024;

/// A store backed by one SQLite database and one writer thread.
///
/// Cloneable and shareable: every clone talks to the same writer thread. Dropping the last clone
/// closes the channel, and the writer thread closes the connection.
#[derive(Clone, Debug)]
pub struct SqliteStore {
    inner: Arc<Handle>,
}

#[derive(Debug)]
struct Handle {
    commands: mpsc::Sender<Command>,
    path: PathBuf,
}

impl SqliteStore {
    /// Opens (creating if absent) the database at `path`, applies migrations, and starts the writer
    /// thread.
    ///
    /// # Errors
    ///
    /// [`PortError`] if the database cannot be opened, a PRAGMA fails, a migration fails, or the
    /// writer thread cannot be spawned.
    pub fn open(path: impl AsRef<Path>) -> Result<Self, PortError> {
        let path = path.as_ref().to_path_buf();
        let connection = open_connection(&path)?;
        let (commands, receiver) = mpsc::channel(COMMAND_CHANNEL_CAPACITY);
        std::thread::Builder::new()
            .name("store-sqlite-writer".to_owned())
            .spawn(move || writer::run(connection, receiver))
            .map_err(|error| {
                PortError::unavailable(PortName::EventStore, "could not start the writer thread")
                    .with_source(error)
            })?;
        Ok(Self {
            inner: Arc::new(Handle { commands, path }),
        })
    }

    /// The database file this store is backed by.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.inner.path
    }

    /// Allocates the next gapless receipt number for a bill (ADR-0025, the `store_server`
    /// authority).
    ///
    /// Idempotent by `bill_id`: allocating twice for one bill returns the same number and does not
    /// advance the counter, so a crash between allocating and appending the `billing.bill.settled`
    /// event reuses the number rather than skipping one. Gapless while this single store authority
    /// is reachable, because every allocation serialises through the one writer thread.
    ///
    /// This is the store's own receipt number, **never** a legal invoice number — that is the
    /// country module's, issued from a pre-allocated range, and the two must never be conflated.
    ///
    /// # Errors
    ///
    /// [`PortError`] if the store cannot be reached or the allocation fails.
    pub async fn allocate_receipt_number(
        &self,
        store_id: StoreId,
        bill_id: BillId,
    ) -> Result<u64, PortError> {
        self.ask(PortName::EventStore, move |reply| {
            Command::AllocateReceipt {
                store_id,
                bill_id,
                reply,
            }
        })
        .await
    }

    /// Allocates the next daily queue number for a tableless order, or returns the one it already
    /// has (ADR-0064, the edge `OrderIn` authority).
    ///
    /// Keyed by `(store_id, business_date)`, so a new business date restarts the sequence at 1 —
    /// the daily reset with no midnight job. Idempotent by `order_id`: allocating twice for one
    /// order returns the same number and does not advance the counter, so a retry after a crash
    /// shouts the same number rather than burning a second one. Collision-free while this single
    /// authority is reachable, because every allocation serialises through the one writer thread.
    ///
    /// # Errors
    ///
    /// [`PortError`] if the store cannot be reached or the allocation fails.
    pub async fn allocate_daily_queue_number(
        &self,
        store_id: StoreId,
        business_date: BusinessDate,
        order_id: OrderId,
    ) -> Result<u64, PortError> {
        self.ask(PortName::OrderIn, move |reply| {
            Command::AllocateQueueNumber {
                store_id,
                business_date,
                order_id,
                reply,
            }
        })
        .await
    }

    /// The queue number an order already holds, or `None` if it was never given one (migration
    /// 0003).
    ///
    /// A read, so a counter screen can show the number without minting one. Keyed by
    /// `(store, order)` with no business date, which is what `queue_allocations` stores — so an
    /// order still open past the day's cutoff is found rather than lost.
    ///
    /// # Errors
    ///
    /// [`PortError`] if the store cannot be reached or the read fails.
    pub async fn daily_queue_number_for(
        &self,
        store_id: StoreId,
        order_id: OrderId,
    ) -> Result<Option<u64>, PortError> {
        self.ask(PortName::OrderIn, move |reply| Command::QueueNumberFor {
            store_id,
            order_id,
            reply,
        })
        .await
    }

    /// Records the store's latest OTA self-test, replacing any earlier one (migration 0006, the
    /// rollback rule of [ADR-0048]).
    ///
    /// `version` is the release that was tested, stored rather than inferred, because the rollback
    /// rule compares it against what the box is *now* running: a failure recorded against a version
    /// the box has since left is history, not a reason to revert.
    ///
    /// [ADR-0048]: ../../../docs/adr/0048-ota-rollout-model.md
    ///
    /// # Errors
    ///
    /// [`PortError`] if the store cannot be reached or the write fails.
    pub async fn record_ota_self_test(
        &self,
        store_id: StoreId,
        version: String,
        passed: bool,
    ) -> Result<(), PortError> {
        self.ask(PortName::CloudSync, move |reply| Command::RecordSelfTest {
            store_id,
            row: SelfTestRow { version, passed },
            reply,
        })
        .await
    }

    /// The store's last OTA self-test as `(version, passed)`, or `None` if it has never recorded one
    /// — a box that has never installed anything, which the rollback rule reads as nothing to revert
    /// from.
    ///
    /// # Errors
    ///
    /// [`PortError`] if the store cannot be reached or the read fails.
    pub async fn last_ota_self_test(
        &self,
        store_id: StoreId,
    ) -> Result<Option<(String, bool)>, PortError> {
        let row = self
            .ask(PortName::CloudSync, move |reply| Command::LastSelfTest {
                store_id,
                reply,
            })
            .await?;
        Ok(row.map(|row| (row.version, row.passed)))
    }

    /// Sends a command to the writer thread and awaits its reply.
    /// [`Self::ask`] for a registry command, so nine call sites need not each wrap the variant and
    /// name the port. Always [`PortName::DeviceRegistry`]: a registry fault must not be reported
    /// under whichever port happens to sit near it in a metric label.
    async fn registry<T, F>(&self, make: F) -> Result<T, PortError>
    where
        T: Send,
        F: FnOnce(oneshot::Sender<Result<T, PortError>>) -> RegistryCommand,
    {
        self.ask(PortName::DeviceRegistry, |reply| {
            Command::Registry(make(reply))
        })
        .await
    }

    async fn ask<T, F>(&self, port: PortName, make: F) -> Result<T, PortError>
    where
        T: Send,
        F: FnOnce(oneshot::Sender<Result<T, PortError>>) -> Command,
    {
        let (reply, outcome) = oneshot::channel();
        self.inner
            .commands
            .send(make(reply))
            .await
            .map_err(|_| writer_gone(port))?;
        outcome.await.map_err(|_| writer_gone(port))?
    }
}

/// Opens a connection, fixes its PRAGMAs once, and brings the schema up to date (ADR-0015).
fn open_connection(path: &Path) -> Result<Connection, PortError> {
    let mut connection = Connection::open(path).map_err(open_error)?;
    connection
        .execute_batch(
            "PRAGMA journal_mode = WAL;
             PRAGMA synchronous = NORMAL;
             PRAGMA foreign_keys = ON;
             PRAGMA busy_timeout = 5000;",
        )
        .map_err(open_error)?;
    migrations::run(&mut connection).map_err(open_error)?;
    Ok(connection)
}

fn open_error(error: rusqlite::Error) -> PortError {
    PortError::unavailable(PortName::EventStore, "could not open the store database")
        .with_source(error)
}

fn writer_gone(port: PortName) -> PortError {
    PortError::unavailable(port, "the store writer thread is gone")
}

impl Transactional for SqliteStore {
    type Tx = SqliteTx;

    async fn begin(&self) -> Result<Self::Tx, PortError> {
        Ok(SqliteTx {
            commands: self.inner.commands.clone(),
            events: Vec::new(),
            config: None,
            intake: None,
            subjects: Vec::new(),
        })
    }
}

impl EventStore for SqliteStore {
    async fn append(
        &self,
        tx: &mut SqliteTx,
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

        let mut outcome = AppendOutcome::default();
        for envelope in events {
            let already_committed = self.contains(envelope.store_id, envelope.event_id).await?;
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
        let (store_id, after, limit) = (query.store_id, query.after, query.limit);
        self.ask(PortName::EventStore, move |reply| Command::Read {
            store_id,
            after,
            limit,
            reply,
        })
        .await
    }

    async fn contains(&self, store_id: StoreId, event_id: EventId) -> Result<bool, PortError> {
        self.ask(PortName::EventStore, move |reply| Command::Contains {
            store_id,
            event_id,
            reply,
        })
        .await
    }

    async fn outbox_batch(
        &self,
        store_id: StoreId,
        after: OutboxPosition,
        limit: NonZeroU32,
    ) -> Result<Vec<OutboxRecord>, PortError> {
        self.ask(PortName::EventStore, move |reply| Command::OutboxBatch {
            store_id,
            after,
            limit,
            reply,
        })
        .await
    }

    async fn acknowledge_outbox(
        &self,
        store_id: StoreId,
        through: OutboxPosition,
    ) -> Result<u64, PortError> {
        self.ask(PortName::EventStore, move |reply| Command::Acknowledge {
            store_id,
            through,
            reply,
        })
        .await
    }

    async fn outbox_depth(&self, store_id: StoreId) -> Result<u64, PortError> {
        self.ask(PortName::EventStore, move |reply| Command::OutboxDepth {
            store_id,
            reply,
        })
        .await
    }
}

impl ConfigStore for SqliteStore {
    async fn current(&self, store_id: StoreId) -> Result<Option<ConfigSnapshot>, PortError> {
        self.ask(PortName::ConfigStore, move |reply| Command::Current {
            store_id,
            reply,
        })
        .await
    }

    async fn last_known_good(
        &self,
        store_id: StoreId,
    ) -> Result<Option<ConfigSnapshot>, PortError> {
        self.ask(PortName::ConfigStore, move |reply| Command::LastKnownGood {
            store_id,
            reply,
        })
        .await
    }

    async fn apply(
        &self,
        tx: &mut SqliteTx,
        update: &ConfigUpdate,
    ) -> Result<ConfigVersionId, PortError> {
        let reached = match update {
            ConfigUpdate::Snapshot(snapshot) => snapshot.config_version_id,
            ConfigUpdate::Delta(delta) => {
                let held = self
                    .current(delta.store_id)
                    .await?
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

/// Renders an id the way [`Ulid`]'s `FromStr` reads it back, so a row round-trips.
fn id_text<T: core::fmt::Display>(id: T) -> String {
    id.to_string()
}

/// Reads an id back out of a row, reporting a corrupt one rather than substituting a default.
///
/// A silently-defaulted id would resolve a token to the zero device, which is worse than an error:
/// it would authenticate. So a row that does not parse is an internal fault with the detail on the
/// (redacted) source, never a value.
fn parse_id<T: From<Ulid>>(text: &str, what: &str) -> Result<T, PortError> {
    text.parse::<Ulid>().map(T::from).map_err(|error| {
        PortError::internal(
            PortName::DeviceRegistry,
            format!("a stored {what} is not a ULID"),
        )
        .with_source(error)
    })
}

/// Reads a stored instant, likewise refusing rather than defaulting.
fn parse_instant(ms: i64, what: &str) -> Result<Timestamp, PortError> {
    Timestamp::from_milliseconds_since_epoch(ms).map_err(|error| {
        PortError::internal(
            PortName::DeviceRegistry,
            format!("a stored {what} is out of range"),
        )
        .with_source(error)
    })
}

/// Reads a stored digest, refusing a row that is not 64 lowercase hex characters.
///
/// A hand-edited or truncated digest must be visibly wrong. Substituting anything would produce a
/// value that matches no token, which looks like a device that mysteriously stopped working.
fn parse_digest(text: &str) -> Result<TokenDigest, PortError> {
    TokenDigest::parse_hex(text).ok_or_else(|| {
        PortError::internal(
            PortName::DeviceRegistry,
            "a stored token digest is not 64 hex characters",
        )
    })
}

fn to_paired(row: &PairedDeviceRow) -> Result<PairedDevice, PortError> {
    Ok(PairedDevice {
        device_id: parse_id(&row.device_id, "device id")?,
        token_digest: parse_digest(&row.token_digest)?,
        paired_at: parse_instant(row.paired_at_ms, "pairing instant")?,
    })
}

fn to_session(row: &DeviceSessionRow) -> Result<DeviceSession, PortError> {
    Ok(DeviceSession {
        device_id: parse_id(&row.device_id, "device id")?,
        employee_id: parse_id(&row.employee_id, "employee id")?,
        signed_in_at: parse_instant(row.signed_in_at_ms, "sign-in instant")?,
        last_seen_at: parse_instant(row.last_seen_at_ms, "last-seen instant")?,
    })
}

impl DeviceRegistry for SqliteStore {
    async fn record_pairing(&self, device: PairedDevice) -> Result<(), PortError> {
        // The digest arrives already computed: this adapter never sees a device token, so it
        // cannot leak one (ADR-0091).
        let row = PairedDeviceRow {
            device_id: id_text(device.device_id),
            token_digest: device.token_digest.to_hex(),
            paired_at_ms: device.paired_at.as_milliseconds_since_epoch(),
        };
        self.registry(move |reply| RegistryCommand::RecordPairing { device: row, reply })
            .await
    }

    async fn device_for_token(&self, digest: TokenDigest) -> Result<Option<DeviceId>, PortError> {
        let token_digest = digest.to_hex();
        let found = self
            .registry(move |reply| RegistryCommand::DeviceForDigest {
                token_digest,
                reply,
            })
            .await?;
        match found {
            Some(text) => Ok(Some(parse_id(&text, "device id")?)),
            None => Ok(None),
        }
    }

    async fn paired_devices(&self) -> Result<Vec<PairedDevice>, PortError> {
        let rows = self
            .registry(|reply| RegistryCommand::PairedDevices { reply })
            .await?;
        rows.iter().map(to_paired).collect()
    }

    async fn revoke_device(&self, device_id: DeviceId) -> Result<(), PortError> {
        let id = id_text(device_id);
        self.registry(move |reply| RegistryCommand::RevokeDevices {
            device_id: Some(id),
            reply,
        })
        .await
    }

    async fn revoke_all_devices(&self) -> Result<(), PortError> {
        self.registry(|reply| RegistryCommand::RevokeDevices {
            device_id: None,
            reply,
        })
        .await
    }

    async fn record_sign_in(&self, session: DeviceSession) -> Result<(), PortError> {
        let row = DeviceSessionRow {
            device_id: id_text(session.device_id),
            employee_id: id_text(session.employee_id),
            signed_in_at_ms: session.signed_in_at.as_milliseconds_since_epoch(),
            last_seen_at_ms: session.last_seen_at.as_milliseconds_since_epoch(),
        };
        self.registry(move |reply| RegistryCommand::RecordSignIn {
            session: row,
            reply,
        })
        .await
    }

    async fn sign_in_for(&self, device_id: DeviceId) -> Result<Option<DeviceSession>, PortError> {
        let id = id_text(device_id);
        let row = self
            .registry(move |reply| RegistryCommand::SignInFor {
                device_id: id,
                reply,
            })
            .await?;
        row.as_ref().map(to_session).transpose()
    }

    async fn sign_ins(&self) -> Result<Vec<DeviceSession>, PortError> {
        let rows = self
            .registry(|reply| RegistryCommand::SignIns { reply })
            .await?;
        rows.iter().map(to_session).collect()
    }

    async fn touch_session(&self, device_id: DeviceId, now: Timestamp) -> Result<(), PortError> {
        let id = id_text(device_id);
        let now_ms = now.as_milliseconds_since_epoch();
        self.registry(move |reply| RegistryCommand::TouchSession {
            device_id: id,
            now_ms,
            reply,
        })
        .await
    }

    async fn clear_sign_in(&self, device_id: DeviceId) -> Result<(), PortError> {
        let id = id_text(device_id);
        self.registry(move |reply| RegistryCommand::ClearSignIn {
            device_id: id,
            reply,
        })
        .await
    }
}

impl SubjectStore for SqliteStore {
    async fn record(
        &self,
        tx: &mut SqliteTx,
        store_id: StoreId,
        subject_id: SubjectId,
        record: &SubjectRecord,
    ) -> Result<(), PortError> {
        // Serialised here so the writer thread stays free of `pos_ports` types — and so the file
        // that moves the bytes never has to know they are somebody's name. Flushed in the settle's
        // own transaction at commit (ADR-0107).
        let fields_json = serde_json::to_string(&record.fields).map_err(|error| {
            PortError::internal(
                PortName::SubjectStore,
                "could not encode the subject record",
            )
            .with_source(error)
        })?;
        tx.subjects.push(SubjectWrite {
            store_id,
            subject_id: subject_id.to_string(),
            collected_at_ms: record.collected_at.as_milliseconds_since_epoch(),
            fields_json,
            masked_at_ms: record.masked_at.map(Timestamp::as_milliseconds_since_epoch),
        });
        Ok(())
    }

    async fn fetch(
        &self,
        store_id: StoreId,
        subject_id: SubjectId,
    ) -> Result<Option<SubjectRecord>, PortError> {
        let key = subject_id.to_string();
        let stored = self
            .ask(PortName::SubjectStore, move |reply| Command::FetchSubject {
                store_id,
                subject_id: key,
                reply,
            })
            .await?;
        stored.as_ref().map(into_record).transpose()
    }

    async fn mask_before(
        &self,
        store_id: StoreId,
        cutoff: Timestamp,
        now: Timestamp,
    ) -> Result<u64, PortError> {
        self.ask(PortName::SubjectStore, move |reply| Command::MaskSubjects {
            store_id,
            cutoff_ms: cutoff.as_milliseconds_since_epoch(),
            now_ms: now.as_milliseconds_since_epoch(),
            reply,
        })
        .await
    }
}

/// Turns a stored subject row back into the port's record.
///
/// A stamp the database cannot represent as a `Timestamp` is a corrupt row rather than a missing
/// value, so it is reported as such instead of being silently read as the epoch — which would put a
/// record decades outside its retention window and have the next sweep scrub it early.
fn into_record(row: &SubjectWrite) -> Result<SubjectRecord, PortError> {
    let port = PortName::SubjectStore;
    let bad_stamp = || PortError::internal(port, "a stored subject has an unrepresentable stamp");
    Ok(SubjectRecord {
        collected_at: Timestamp::from_milliseconds_since_epoch(row.collected_at_ms)
            .map_err(|_ignored| bad_stamp())?,
        fields: serde_json::from_str(&row.fields_json).map_err(|error| {
            PortError::internal(port, "could not decode a stored subject record").with_source(error)
        })?,
        masked_at: row
            .masked_at_ms
            .map(Timestamp::from_milliseconds_since_epoch)
            .transpose()
            .map_err(|_ignored| bad_stamp())?,
    })
}

impl IntakeLedger for SqliteStore {
    async fn record(
        &self,
        tx: &mut SqliteTx,
        store_id: StoreId,
        sales_channel: &str,
        external_reference: &str,
        record: &IntakeRecord,
    ) -> Result<(), PortError> {
        // Serialise here so the writer thread stays free of `pos_ports` types; the row is flushed
        // in the order's own transaction at commit (ADR-0064).
        let record_json = serde_json::to_string(record).map_err(|error| {
            PortError::internal(PortName::IntakeLedger, "could not encode the intake record")
                .with_source(error)
        })?;
        tx.intake = Some(IntakeWrite {
            store_id,
            sales_channel: sales_channel.to_owned(),
            external_reference: external_reference.to_owned(),
            record_json,
        });
        Ok(())
    }

    async fn look_up(
        &self,
        store_id: StoreId,
        sales_channel: &str,
        external_reference: &str,
    ) -> Result<Option<IntakeRecord>, PortError> {
        let (sales_channel, external_reference) =
            (sales_channel.to_owned(), external_reference.to_owned());
        let stored = self
            .ask(PortName::IntakeLedger, move |reply| Command::LookUpIntake {
                store_id,
                sales_channel,
                external_reference,
                reply,
            })
            .await?;
        match stored {
            Some(json) => Ok(Some(serde_json::from_str(&json).map_err(|error| {
                PortError::internal(
                    PortName::IntakeLedger,
                    "could not decode a stored intake record",
                )
                .with_source(error)
            })?)),
            None => Ok(None),
        }
    }
}
