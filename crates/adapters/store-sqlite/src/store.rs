// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! [`SqliteStore`]: `open`, the writer-thread bridge, and the three port implementations.

use core::num::NonZeroU32;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use rusqlite::Connection;
use tokio::sync::{mpsc, oneshot};

use pos_ports::config_store::{ConfigSnapshot, ConfigStore, ConfigUpdate};
use pos_ports::event_store::{AppendOutcome, EventQuery, EventStore, OutboxPosition, OutboxRecord};
use pos_ports::intake_ledger::{IntakeLedger, IntakeRecord};
use pos_ports::{PortError, PortName, Transactional};
use pos_proto::envelope::{EventEnvelope, RawPayload};
use pos_proto::ids::{BillId, ConfigVersionId, EventId, OrderId, StoreId};
use pos_proto::time::BusinessDate;

use crate::migrations;
use crate::tx::SqliteTx;
use crate::writer::{self, Command, IntakeWrite};

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

    /// Sends a command to the writer thread and awaits its reply.
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
            PortError::internal(PortName::OrderIn, "could not encode the intake record")
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
            .ask(PortName::OrderIn, move |reply| Command::LookUpIntake {
                store_id,
                sales_channel,
                external_reference,
                reply,
            })
            .await?;
        match stored {
            Some(json) => Ok(Some(serde_json::from_str(&json).map_err(|error| {
                PortError::internal(PortName::OrderIn, "could not decode a stored intake record")
                    .with_source(error)
            })?)),
            None => Ok(None),
        }
    }
}
