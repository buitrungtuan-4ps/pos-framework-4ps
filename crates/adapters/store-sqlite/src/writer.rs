// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The single writer thread (ADR-0015).
//!
//! One OS thread owns the one `Connection`. Every operation — read and write — arrives as a
//! [`Command`] carrying a `oneshot` reply, so the async port methods stay `async fn` while the
//! blocking SQLite calls happen here, off the executor. Serialising through one thread is the whole
//! write-concurrency story: no `SQLITE_BUSY`, and the outbox position is a monotone rowid assigned
//! inside the commit transaction.

use core::num::NonZeroU32;

use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use tokio::sync::{mpsc, oneshot};

use pos_ports::config_store::{ConfigSnapshot, ConfigUpdate};
use pos_ports::event_store::{OutboxPosition, OutboxRecord};
use pos_ports::{PortError, PortName};
use pos_proto::envelope::{EventEnvelope, RawPayload};
use pos_proto::ids::{BillId, EventId, StoreId};

/// How many undelivered events the store holds before pushing back — mirrors the fake, so
/// back-pressure behaves identically in tests and in the field.
pub const OUTBOX_CAPACITY: usize = 10_000;

/// A unit of work for the writer thread. Every variant carries the channel its result returns on.
pub(crate) enum Command {
    /// Flush a transaction's buffered events and config update in one SQLite transaction.
    Commit {
        events: Vec<EventEnvelope<RawPayload>>,
        config: Option<ConfigUpdate>,
        reply: oneshot::Sender<Result<(), PortError>>,
    },
    /// Read a store's events, ascending by `event_id`, after an optional cursor.
    Read {
        store_id: StoreId,
        after: Option<EventId>,
        limit: NonZeroU32,
        reply: oneshot::Sender<Result<Vec<EventEnvelope<RawPayload>>, PortError>>,
    },
    /// Whether a store already holds an event.
    Contains {
        store_id: StoreId,
        event_id: EventId,
        reply: oneshot::Sender<Result<bool, PortError>>,
    },
    /// A page of the outbox in commit order, after a position.
    OutboxBatch {
        store_id: StoreId,
        after: OutboxPosition,
        limit: NonZeroU32,
        reply: oneshot::Sender<Result<Vec<OutboxRecord>, PortError>>,
    },
    /// Remove every outbox record through a position; returns how many went.
    Acknowledge {
        store_id: StoreId,
        through: OutboxPosition,
        reply: oneshot::Sender<Result<u64, PortError>>,
    },
    /// How many undelivered events a store has.
    OutboxDepth {
        store_id: StoreId,
        reply: oneshot::Sender<Result<u64, PortError>>,
    },
    /// The config version a store currently runs.
    Current {
        store_id: StoreId,
        reply: oneshot::Sender<Result<Option<ConfigSnapshot>, PortError>>,
    },
    /// The last config version that applied cleanly.
    LastKnownGood {
        store_id: StoreId,
        reply: oneshot::Sender<Result<Option<ConfigSnapshot>, PortError>>,
    },
    /// Allocate (or return the already-allocated) gapless receipt number for a bill.
    AllocateReceipt {
        store_id: StoreId,
        bill_id: BillId,
        reply: oneshot::Sender<Result<u64, PortError>>,
    },
}

/// The writer loop: drain commands until every sender is gone, then close the connection.
///
/// A `send` that fails means the caller's future was dropped before its reply arrived — the caller
/// no longer cares, so the reply is discarded.
pub(crate) fn run(mut conn: Connection, mut rx: mpsc::Receiver<Command>) {
    while let Some(command) = rx.blocking_recv() {
        match command {
            Command::Commit {
                events,
                config,
                reply,
            } => {
                let _ = reply.send(commit(&mut conn, &events, config));
            }
            Command::Read {
                store_id,
                after,
                limit,
                reply,
            } => {
                let _ = reply.send(read(&conn, store_id, after, limit));
            }
            Command::Contains {
                store_id,
                event_id,
                reply,
            } => {
                let _ = reply.send(contains(&conn, store_id, event_id));
            }
            Command::OutboxBatch {
                store_id,
                after,
                limit,
                reply,
            } => {
                let _ = reply.send(outbox_batch(&conn, store_id, after, limit));
            }
            Command::Acknowledge {
                store_id,
                through,
                reply,
            } => {
                let _ = reply.send(acknowledge(&conn, store_id, through));
            }
            Command::OutboxDepth { store_id, reply } => {
                let _ = reply.send(outbox_depth(&conn, store_id));
            }
            Command::Current { store_id, reply } => {
                let _ = reply.send(snapshot(&conn, "config_current", store_id));
            }
            Command::LastKnownGood { store_id, reply } => {
                let _ = reply.send(snapshot(&conn, "config_last_known_good", store_id));
            }
            Command::AllocateReceipt {
                store_id,
                bill_id,
                reply,
            } => {
                let _ = reply.send(allocate_receipt(&mut conn, store_id, bill_id));
            }
        }
    }
}

/// An unexpected SQLite failure, its detail kept on the (redacted) source rather than the message.
fn db_error(port: PortName, error: rusqlite::Error) -> PortError {
    PortError::internal(port, "sqlite operation failed").with_source(error)
}

/// A serialisation failure — an envelope or snapshot that would not round-trip.
fn json_error(port: PortName, error: serde_json::Error) -> PortError {
    PortError::internal(port, "could not (de)serialise a stored value").with_source(error)
}

fn commit(
    conn: &mut Connection,
    events: &[EventEnvelope<RawPayload>],
    config: Option<ConfigUpdate>,
) -> Result<(), PortError> {
    let port = PortName::EventStore;
    let tx = conn
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(|error| db_error(port, error))?;

    for envelope in events {
        let store_id = envelope.store_id.to_string();
        let event_id = envelope.event_id.to_string();
        let json = serde_json::to_string(envelope).map_err(|error| json_error(port, error))?;

        // INSERT OR IGNORE is the idempotency: a stored event_id keeps its row, the incoming copy is
        // discarded without a byte comparison (ADR-0026 §5). Only a genuinely new row gets an outbox
        // entry, so a replay never re-queues an event.
        let inserted = tx
            .execute(
                "INSERT OR IGNORE INTO events (store_id, event_id, envelope) VALUES (?1, ?2, ?3)",
                params![store_id, event_id, json],
            )
            .map_err(|error| db_error(port, error))?;
        if inserted > 0 {
            let depth: i64 = tx
                .query_row(
                    "SELECT COUNT(*) FROM outbox WHERE store_id = ?1",
                    params![store_id],
                    |row| row.get(0),
                )
                .map_err(|error| db_error(port, error))?;
            if usize::try_from(depth).unwrap_or(usize::MAX) >= OUTBOX_CAPACITY {
                // Returning here drops `tx`, which rolls the whole transaction back — nothing partial.
                return Err(PortError::resource_exhausted(
                    port,
                    "the outbox is at capacity",
                ));
            }
            tx.execute(
                "INSERT INTO outbox (store_id, envelope) VALUES (?1, ?2)",
                params![store_id, json],
            )
            .map_err(|error| db_error(port, error))?;
        }
    }

    if let Some(update) = config {
        let snapshot = match update {
            ConfigUpdate::Snapshot(snapshot) => snapshot,
            // A delta's target document is its patch: merging is P7's decision, and inventing a
            // patch format here would make the edge disagree with the cloud. Mirrors the fake.
            ConfigUpdate::Delta(delta) => ConfigSnapshot {
                config_version_id: delta.to_config_version_id,
                store_id: delta.store_id,
                document: delta.patch,
            },
        };
        let store_id = snapshot.store_id.to_string();
        let json = serde_json::to_string(&snapshot)
            .map_err(|error| json_error(PortName::ConfigStore, error))?;
        for table in ["config_current", "config_last_known_good"] {
            let sql = format!(
                "INSERT INTO {table} (store_id, snapshot) VALUES (?1, ?2)
                 ON CONFLICT (store_id) DO UPDATE SET snapshot = excluded.snapshot"
            );
            tx.execute(&sql, params![store_id, json])
                .map_err(|error| db_error(PortName::ConfigStore, error))?;
        }
    }

    tx.commit().map_err(|error| db_error(port, error))
}

fn read(
    conn: &Connection,
    store_id: StoreId,
    after: Option<EventId>,
    limit: NonZeroU32,
) -> Result<Vec<EventEnvelope<RawPayload>>, PortError> {
    let port = PortName::EventStore;
    // The empty string sorts below every canonical 26-character ULID, so a `None` cursor becomes
    // "greater than nothing" — one query, no branch. The bound is exclusive, so paging never repeats.
    let after_key = after.map(|id| id.to_string()).unwrap_or_default();
    let limit = i64::from(limit.get());
    let mut statement = conn
        .prepare(
            "SELECT envelope FROM events
             WHERE store_id = ?1 AND event_id > ?2
             ORDER BY event_id ASC LIMIT ?3",
        )
        .map_err(|error| db_error(port, error))?;
    let rows = statement
        .query_map(params![store_id.to_string(), after_key, limit], |row| {
            row.get::<_, String>(0)
        })
        .map_err(|error| db_error(port, error))?;

    let mut events = Vec::new();
    for row in rows {
        let json = row.map_err(|error| db_error(port, error))?;
        events.push(serde_json::from_str(&json).map_err(|error| json_error(port, error))?);
    }
    Ok(events)
}

fn contains(conn: &Connection, store_id: StoreId, event_id: EventId) -> Result<bool, PortError> {
    let port = PortName::EventStore;
    conn.query_row(
        "SELECT EXISTS (SELECT 1 FROM events WHERE store_id = ?1 AND event_id = ?2)",
        params![store_id.to_string(), event_id.to_string()],
        |row| row.get::<_, i64>(0),
    )
    .map(|present| present != 0)
    .map_err(|error| db_error(port, error))
}

fn outbox_batch(
    conn: &Connection,
    store_id: StoreId,
    after: OutboxPosition,
    limit: NonZeroU32,
) -> Result<Vec<OutboxRecord>, PortError> {
    let port = PortName::EventStore;
    let after = i64::try_from(after.get()).unwrap_or(i64::MAX);
    let limit = i64::from(limit.get());
    let mut statement = conn
        .prepare(
            "SELECT position, envelope FROM outbox
             WHERE store_id = ?1 AND position > ?2
             ORDER BY position ASC LIMIT ?3",
        )
        .map_err(|error| db_error(port, error))?;
    let rows = statement
        .query_map(params![store_id.to_string(), after, limit], |row| {
            Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?))
        })
        .map_err(|error| db_error(port, error))?;

    let mut records = Vec::new();
    for row in rows {
        let (position, json) = row.map_err(|error| db_error(port, error))?;
        let position = OutboxPosition::new(u64::try_from(position).unwrap_or(0));
        let envelope = serde_json::from_str(&json).map_err(|error| json_error(port, error))?;
        records.push(OutboxRecord { position, envelope });
    }
    Ok(records)
}

fn acknowledge(
    conn: &Connection,
    store_id: StoreId,
    through: OutboxPosition,
) -> Result<u64, PortError> {
    let port = PortName::EventStore;
    let through = i64::try_from(through.get()).unwrap_or(i64::MAX);
    let removed = conn
        .execute(
            "DELETE FROM outbox WHERE store_id = ?1 AND position <= ?2",
            params![store_id.to_string(), through],
        )
        .map_err(|error| db_error(port, error))?;
    Ok(u64::try_from(removed).unwrap_or(u64::MAX))
}

fn outbox_depth(conn: &Connection, store_id: StoreId) -> Result<u64, PortError> {
    let port = PortName::EventStore;
    let depth: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM outbox WHERE store_id = ?1",
            params![store_id.to_string()],
            |row| row.get(0),
        )
        .map_err(|error| db_error(port, error))?;
    Ok(u64::try_from(depth).unwrap_or(u64::MAX))
}

/// Allocates the next gapless receipt number for a bill, or returns the one it already has
/// (ADR-0025, `store_server` authority).
///
/// One `IMMEDIATE` transaction does the whole read-modify-write, so concurrent allocations — which
/// all funnel through this one writer thread anyway — cannot interleave: the sequence is gapless and
/// collision-free. Idempotency is the `receipt_allocations` row: a bill that already has a number
/// gets it back without advancing the counter, so retrying a settle after a crash reuses the number
/// rather than skipping one.
fn allocate_receipt(
    conn: &mut Connection,
    store_id: StoreId,
    bill_id: BillId,
) -> Result<u64, PortError> {
    let port = PortName::EventStore;
    let store = store_id.to_string();
    let bill = bill_id.to_string();

    let tx = conn
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(|error| db_error(port, error))?;

    let existing: Option<i64> = tx
        .query_row(
            "SELECT receipt_number FROM receipt_allocations WHERE store_id = ?1 AND bill_id = ?2",
            params![store, bill],
            |row| row.get(0),
        )
        .optional()
        .map_err(|error| db_error(port, error))?;

    let number = if let Some(number) = existing {
        number
    } else {
        // Ensure a counter row (starting at 1), read the number it will hand out, advance it, and
        // record the allocation — all inside this transaction, so a rollback consumes nothing.
        tx.execute(
            "INSERT INTO receipt_counter (store_id, next_number) VALUES (?1, 1)
             ON CONFLICT (store_id) DO NOTHING",
            params![store],
        )
        .map_err(|error| db_error(port, error))?;
        let allocated: i64 = tx
            .query_row(
                "SELECT next_number FROM receipt_counter WHERE store_id = ?1",
                params![store],
                |row| row.get(0),
            )
            .map_err(|error| db_error(port, error))?;
        tx.execute(
            "UPDATE receipt_counter SET next_number = next_number + 1 WHERE store_id = ?1",
            params![store],
        )
        .map_err(|error| db_error(port, error))?;
        tx.execute(
            "INSERT INTO receipt_allocations (store_id, bill_id, receipt_number) VALUES (?1, ?2, ?3)",
            params![store, bill, allocated],
        )
        .map_err(|error| db_error(port, error))?;
        allocated
    };

    tx.commit().map_err(|error| db_error(port, error))?;
    Ok(u64::try_from(number).unwrap_or(0))
}

fn snapshot(
    conn: &Connection,
    table: &str,
    store_id: StoreId,
) -> Result<Option<ConfigSnapshot>, PortError> {
    let port = PortName::ConfigStore;
    let sql = format!("SELECT snapshot FROM {table} WHERE store_id = ?1");
    let json: Option<String> = conn
        .query_row(&sql, params![store_id.to_string()], |row| row.get(0))
        .optional()
        .map_err(|error| db_error(port, error))?;
    match json {
        Some(json) => Ok(Some(
            serde_json::from_str(&json).map_err(|error| json_error(port, error))?,
        )),
        None => Ok(None),
    }
}
