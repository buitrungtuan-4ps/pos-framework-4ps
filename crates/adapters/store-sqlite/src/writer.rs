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
use pos_ports::subject_store::REDACTION;
use pos_ports::{PortError, PortName};
use pos_proto::envelope::{EventEnvelope, RawPayload};
use pos_proto::ids::{BillId, EventId, OrderId, StoreId};
use pos_proto::time::BusinessDate;

/// How many undelivered events the store holds before pushing back — mirrors the fake, so
/// back-pressure behaves identically in tests and in the field.
pub const OUTBOX_CAPACITY: usize = 10_000;

/// An inbound-order idempotency row buffered for the order's transaction (ADR-0064). The record is
/// pre-serialised to JSON by the store so the writer thread stays free of `pos_ports` types.
#[derive(Debug)]
pub(crate) struct IntakeWrite {
    pub(crate) store_id: StoreId,
    pub(crate) sales_channel: String,
    pub(crate) external_reference: String,
    pub(crate) record_json: String,
}

/// A subject row buffered for the settle's transaction
/// ([ADR-0107](../../../docs/adr/0107-the-buyer-is-a-subject.md)). The fields arrive pre-serialised
/// to JSON by the store, so the writer thread stays free of `pos_ports` types — and so this file
/// never has to know that the JSON it is moving is somebody's name.
#[derive(Debug)]
pub(crate) struct SubjectWrite {
    pub(crate) store_id: StoreId,
    pub(crate) subject_id: String,
    pub(crate) collected_at_ms: i64,
    pub(crate) fields_json: String,
    pub(crate) masked_at_ms: Option<i64>,
}

/// A device-registry row in the writer thread's own terms (ADR-0091).
///
/// Ids and the digest arrive already rendered as text and the instants as milliseconds, so this
/// thread stays free of `pos_ports` and `pos_proto` types — the same reason [`IntakeWrite`] carries
/// pre-serialised JSON. The digest is a SHA-256 of the device token computed by the caller; the
/// token itself never reaches this file.
#[derive(Debug)]
pub(crate) struct PairedDeviceRow {
    pub(crate) device_id: String,
    pub(crate) token_digest: String,
    pub(crate) paired_at_ms: i64,
}

/// A sign-in row, likewise.
#[derive(Debug)]
pub(crate) struct DeviceSessionRow {
    pub(crate) device_id: String,
    pub(crate) employee_id: String,
    pub(crate) signed_in_at_ms: i64,
    pub(crate) last_seen_at_ms: i64,
}

/// A unit of work for the writer thread. Every variant carries the channel its result returns on.
pub(crate) enum Command {
    /// Flush a transaction's buffered events, config update, intake row and subject rows in one
    /// SQLite transaction.
    Commit {
        events: Vec<EventEnvelope<RawPayload>>,
        config: Option<ConfigUpdate>,
        intake: Option<IntakeWrite>,
        subjects: Vec<SubjectWrite>,
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
    /// Allocate (or return the already-allocated) daily queue number for a tableless order.
    AllocateQueueNumber {
        store_id: StoreId,
        business_date: BusinessDate,
        order_id: OrderId,
        reply: oneshot::Sender<Result<u64, PortError>>,
    },
    /// The queue number an order was already given, or `None`. A **read**: unlike
    /// [`Command::AllocateQueueNumber`] it never mints one, so a screen can show the number the
    /// counter shouted without a GET minting numbers for orders that have none.
    QueueNumberFor {
        store_id: StoreId,
        order_id: OrderId,
        reply: oneshot::Sender<Result<Option<u64>, PortError>>,
    },
    /// The intake-ledger record a `(store, sales_channel, external_reference)` already produced, as
    /// stored JSON, or `None`.
    LookUpIntake {
        store_id: StoreId,
        sales_channel: String,
        external_reference: String,
        reply: oneshot::Sender<Result<Option<String>, PortError>>,
    },
    /// Record the store's latest OTA self-test, replacing any earlier one (migration 0006).
    RecordSelfTest {
        store_id: StoreId,
        row: SelfTestRow,
        reply: oneshot::Sender<Result<(), PortError>>,
    },
    /// The store's last OTA self-test, or `None` if it has never recorded one.
    LastSelfTest {
        store_id: StoreId,
        reply: oneshot::Sender<Result<Option<SelfTestRow>, PortError>>,
    },
    /// One subject's stored row (ADR-0107), or `None`. Deliberately by id: there is no
    /// read-them-all, because a query that could enumerate personal data is a query that can export
    /// it.
    FetchSubject {
        store_id: StoreId,
        subject_id: String,
        reply: oneshot::Sender<Result<Option<SubjectWrite>, PortError>>,
    },
    /// The retention sweep: scrub every unmasked subject collected at or before a cutoff, returning
    /// how many rows changed (ADR-0107, ADR-0035).
    MaskSubjects {
        store_id: StoreId,
        cutoff_ms: i64,
        now_ms: i64,
        reply: oneshot::Sender<Result<u64, PortError>>,
    },
    /// Anything in the device registry (ADR-0091), grouped because it is a distinct concern from
    /// the event store and keeps this enum's dispatch readable.
    Registry(RegistryCommand),
}

/// The store's last self-test as it sits in `ota_self_test` (migration 0006): the release that was
/// tested and whether it passed. The version travels as its wire string, so this row stays a plain
/// data carrier and `pos-edge` owns the parse back into a `ReleaseVersion`.
#[derive(Debug, Clone)]
pub(crate) struct SelfTestRow {
    pub(crate) version: String,
    pub(crate) passed: bool,
}

/// A device-registry unit of work (ADR-0091).
#[derive(Debug)]
pub(crate) enum RegistryCommand {
    /// Record (or replace) a paired device.
    RecordPairing {
        device: PairedDeviceRow,
        reply: oneshot::Sender<Result<(), PortError>>,
    },
    /// The device a token digest was issued to.
    DeviceForDigest {
        token_digest: String,
        reply: oneshot::Sender<Result<Option<String>, PortError>>,
    },
    /// Every paired device.
    PairedDevices {
        reply: oneshot::Sender<Result<Vec<PairedDeviceRow>, PortError>>,
    },
    /// Retire one device, or — with `device_id` absent — every device.
    RevokeDevices {
        device_id: Option<String>,
        reply: oneshot::Sender<Result<(), PortError>>,
    },
    /// Record (or replace) a sign-in.
    RecordSignIn {
        session: DeviceSessionRow,
        reply: oneshot::Sender<Result<(), PortError>>,
    },
    /// The sign-in on one device.
    SignInFor {
        device_id: String,
        reply: oneshot::Sender<Result<Option<DeviceSessionRow>, PortError>>,
    },
    /// Every sign-in.
    SignIns {
        reply: oneshot::Sender<Result<Vec<DeviceSessionRow>, PortError>>,
    },
    /// Move a device's `last_seen_at` forward. A no-op when it has no session.
    TouchSession {
        device_id: String,
        now_ms: i64,
        reply: oneshot::Sender<Result<(), PortError>>,
    },
    /// End the sign-in on one device.
    ClearSignIn {
        device_id: String,
        reply: oneshot::Sender<Result<(), PortError>>,
    },
}

/// The writer loop: drain commands until every sender is gone, then close the connection.
///
/// A `send` that fails means the caller's future was dropped before its reply arrived — the caller
/// no longer cares, so the reply is discarded.
#[expect(
    clippy::too_many_lines,
    reason = "a dispatch table: one arm per command, each forwarding to its own function. Its \
              length is the number of commands, and splitting it would scatter the \
              command-to-handler mapping this loop exists to make obvious in one place."
)]
pub(crate) fn run(mut conn: Connection, mut rx: mpsc::Receiver<Command>) {
    while let Some(command) = rx.blocking_recv() {
        match command {
            Command::Commit {
                events,
                config,
                intake,
                subjects,
                reply,
            } => {
                let _ = reply.send(commit(&mut conn, &events, config, intake, &subjects));
            }
            Command::FetchSubject {
                store_id,
                subject_id,
                reply,
            } => {
                let _ = reply.send(fetch_subject(&conn, store_id, &subject_id));
            }
            Command::MaskSubjects {
                store_id,
                cutoff_ms,
                now_ms,
                reply,
            } => {
                let _ = reply.send(mask_subjects(&conn, store_id, cutoff_ms, now_ms));
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
            Command::AllocateQueueNumber {
                store_id,
                business_date,
                order_id,
                reply,
            } => {
                let _ = reply.send(allocate_queue_number(
                    &mut conn,
                    store_id,
                    business_date,
                    order_id,
                ));
            }
            Command::QueueNumberFor {
                store_id,
                order_id,
                reply,
            } => {
                let _ = reply.send(queue_number_for(&conn, store_id, order_id));
            }
            Command::RecordSelfTest {
                store_id,
                row,
                reply,
            } => {
                let _ = reply.send(record_self_test(&conn, store_id, &row));
            }
            Command::LastSelfTest { store_id, reply } => {
                let _ = reply.send(last_self_test(&conn, store_id));
            }
            Command::LookUpIntake {
                store_id,
                sales_channel,
                external_reference,
                reply,
            } => {
                let _ = reply.send(look_up_intake(
                    &conn,
                    store_id,
                    &sales_channel,
                    &external_reference,
                ));
            }
            Command::Registry(command) => run_registry(&mut conn, command),
        }
    }
}

/// Dispatches one device-registry command. Separate from [`run`] so neither match grows past the
/// line budget, and because the two concerns share nothing but the connection.
fn run_registry(conn: &mut Connection, command: RegistryCommand) {
    match command {
        RegistryCommand::RecordPairing { device, reply } => {
            let _ = reply.send(record_pairing(conn, &device));
        }
        RegistryCommand::DeviceForDigest {
            token_digest,
            reply,
        } => {
            let _ = reply.send(device_for_digest(conn, &token_digest));
        }
        RegistryCommand::PairedDevices { reply } => {
            let _ = reply.send(paired_devices(conn));
        }
        RegistryCommand::RevokeDevices { device_id, reply } => {
            let _ = reply.send(revoke_devices(conn, device_id.as_deref()));
        }
        RegistryCommand::RecordSignIn { session, reply } => {
            let _ = reply.send(record_sign_in(conn, &session));
        }
        RegistryCommand::SignInFor { device_id, reply } => {
            let _ = reply.send(sign_in_for(conn, &device_id));
        }
        RegistryCommand::SignIns { reply } => {
            let _ = reply.send(sign_ins(conn));
        }
        RegistryCommand::TouchSession {
            device_id,
            now_ms,
            reply,
        } => {
            let _ = reply.send(touch_session(conn, &device_id, now_ms));
        }
        RegistryCommand::ClearSignIn { device_id, reply } => {
            let _ = reply.send(clear_sign_in(conn, &device_id));
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
    intake: Option<IntakeWrite>,
    subjects: &[SubjectWrite],
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

    if let Some(intake) = intake {
        // A PLAIN insert, not insert-or-ignore: a second order racing in on the same key must fail
        // and roll the whole transaction back (this one writer thread serialises the two), never
        // duplicate. The caller resolves the loss by looking the key up (ADR-0064). Returning here
        // drops `tx`, rolling back the events written above with it — atomic by construction.
        let result = tx.execute(
            "INSERT INTO intake_ledger (store_id, sales_channel, external_reference, record)
             VALUES (?1, ?2, ?3, ?4)",
            params![
                intake.store_id.to_string(),
                intake.sales_channel,
                intake.external_reference,
                intake.record_json,
            ],
        );
        if let Err(error) = result {
            return Err(intake_conflict_or_db_error(error));
        }
    }

    for subject in subjects {
        // Upsert rather than a plain insert: ids are minted fresh per record, so this is not a merge
        // policy anyone relies on — it is the absence of a failure mode, because a retried write must
        // not be able to fail a transaction that is otherwise sound (ADR-0107).
        tx.execute(
            "INSERT INTO subjects (store_id, subject_id, collected_at, fields, masked_at)
             VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT (store_id, subject_id) DO UPDATE SET
                 collected_at = excluded.collected_at,
                 fields       = excluded.fields,
                 masked_at    = excluded.masked_at",
            params![
                subject.store_id.to_string(),
                subject.subject_id,
                subject.collected_at_ms,
                subject.fields_json,
                subject.masked_at_ms,
            ],
        )
        .map_err(|error| db_error(PortName::SubjectStore, error))?;
    }

    tx.commit().map_err(|error| db_error(port, error))
}

/// One subject row, by id, scoped to the store that recorded it.
fn fetch_subject(
    conn: &Connection,
    store_id: StoreId,
    subject_id: &str,
) -> Result<Option<SubjectWrite>, PortError> {
    let port = PortName::SubjectStore;
    let mut statement = conn
        .prepare_cached(
            "SELECT collected_at, fields, masked_at FROM subjects
             WHERE store_id = ?1 AND subject_id = ?2",
        )
        .map_err(|error| db_error(port, error))?;
    let mut rows = statement
        .query(params![store_id.to_string(), subject_id])
        .map_err(|error| db_error(port, error))?;
    let Some(row) = rows.next().map_err(|error| db_error(port, error))? else {
        return Ok(None);
    };
    Ok(Some(SubjectWrite {
        store_id,
        subject_id: subject_id.to_owned(),
        collected_at_ms: row.get(0).map_err(|error| db_error(port, error))?,
        fields_json: row.get(1).map_err(|error| db_error(port, error))?,
        masked_at_ms: row.get(2).map_err(|error| db_error(port, error))?,
    }))
}

/// The retention sweep (ADR-0107, ADR-0035): replace every field *value* with the redaction
/// sentinel and stamp `masked_at`, for unmasked rows collected at or before the cutoff.
///
/// `json_group_object` rebuilds the document from its own keys, so the field names survive and the
/// values do not — which is what keeps "what kind of data was held" knowable after the data is gone.
/// The `masked_at IS NULL` guard is what makes a second sweep report zero rather than re-stamping
/// yesterday's masking.
fn mask_subjects(
    conn: &Connection,
    store_id: StoreId,
    cutoff_ms: i64,
    now_ms: i64,
) -> Result<u64, PortError> {
    let port = PortName::SubjectStore;
    let changed = conn
        .execute(
            "UPDATE subjects SET
                 fields = (SELECT json_group_object(key, ?4) FROM json_each(subjects.fields)),
                 masked_at = ?3
             WHERE store_id = ?1 AND collected_at <= ?2 AND masked_at IS NULL",
            params![store_id.to_string(), cutoff_ms, now_ms, REDACTION],
        )
        .map_err(|error| db_error(port, error))?;
    Ok(u64::try_from(changed).unwrap_or(0))
}

/// A failed intake insert is either the key already existing — the concurrent-duplicate case the
/// plain insert exists to catch, reported as [`PortError::already_exists`] so the caller re-resolves
/// with a look-up — or a genuine store fault.
fn intake_conflict_or_db_error(error: rusqlite::Error) -> PortError {
    let port = PortName::IntakeLedger;
    if let rusqlite::Error::SqliteFailure(failure, _) = &error
        && failure.code == rusqlite::ErrorCode::ConstraintViolation
    {
        return PortError::already_exists(port, "an order already exists for this reference")
            .with_source(error);
    }
    db_error(port, error)
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

/// Allocates the next daily queue number for a tableless order, or returns the one it already has
/// (ADR-0064, the edge `OrderIn` authority).
///
/// The counter is keyed by `(store, business_date)`, so a business date the counter has never seen
/// starts at 1 — the daily reset, with no midnight job. One `IMMEDIATE` transaction does the whole
/// read-modify-write, and every allocation funnels through this one writer thread, so two channels
/// delivering at once are handed distinct numbers. Idempotency is the `queue_allocations` row: an
/// order that already has a number gets it back without advancing the counter, so a retry after a
/// crash shouts the same number rather than burning a second one.
/// Records the store's latest self-test, replacing any earlier one (migration 0006).
///
/// An upsert rather than an append: the rollback rule reads only the most recent verdict, and the
/// fleet's history is the cloud's through `CloudSync::report` (ADR-0078). Reported under
/// [`PortName::CloudSync`] because that is the port this state exists to serve — the OTA path — and a
/// fault here must not surface under whichever port happens to sit near it in a metric label.
fn record_self_test(
    conn: &Connection,
    store_id: StoreId,
    row: &SelfTestRow,
) -> Result<(), PortError> {
    let port = PortName::CloudSync;
    conn.execute(
        "INSERT INTO ota_self_test (store_id, version, passed, recorded_time)
         VALUES (?1, ?2, ?3, datetime('now'))
         ON CONFLICT (store_id) DO UPDATE SET
             version = excluded.version,
             passed = excluded.passed,
             recorded_time = excluded.recorded_time",
        params![store_id.to_string(), row.version, i64::from(row.passed)],
    )
    .map_err(|error| db_error(port, error))?;
    Ok(())
}

/// The store's last self-test, or `None` if it has never recorded one — a box that has never
/// installed anything, which the rollback rule reads as "nothing to revert from".
fn last_self_test(conn: &Connection, store_id: StoreId) -> Result<Option<SelfTestRow>, PortError> {
    let port = PortName::CloudSync;
    conn.query_row(
        "SELECT version, passed FROM ota_self_test WHERE store_id = ?1",
        params![store_id.to_string()],
        |row| {
            Ok(SelfTestRow {
                version: row.get(0)?,
                passed: row.get::<_, i64>(1)? != 0,
            })
        },
    )
    .optional()
    .map_err(|error| db_error(port, error))
}

/// The queue number an order already holds, or `None` if it was never given one.
///
/// Read-only, and keyed by `(store, order)` alone — `queue_allocations` carries no business date
/// (migration 0003 records "an order id already names its store"), so a counter order left unpaid
/// past the cutoff is still found the next morning. Asking
/// [`allocate_queue_number`] instead would answer too, since it is idempotent by order, but it
/// would *mint* a number for an order that has none — a write on a read path.
fn queue_number_for(
    conn: &Connection,
    store_id: StoreId,
    order_id: OrderId,
) -> Result<Option<u64>, PortError> {
    let port = PortName::OrderIn;
    let number: Option<i64> = conn
        .query_row(
            "SELECT queue_number FROM queue_allocations WHERE store_id = ?1 AND order_id = ?2",
            params![store_id.to_string(), order_id.to_string()],
            |row| row.get(0),
        )
        .optional()
        .map_err(|error| db_error(port, error))?;
    Ok(number.map(|value| u64::try_from(value).unwrap_or(0)))
}

fn allocate_queue_number(
    conn: &mut Connection,
    store_id: StoreId,
    business_date: BusinessDate,
    order_id: OrderId,
) -> Result<u64, PortError> {
    let port = PortName::OrderIn;
    let store = store_id.to_string();
    let date = business_date.to_string();
    let order = order_id.to_string();

    let tx = conn
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(|error| db_error(port, error))?;

    let existing: Option<i64> = tx
        .query_row(
            "SELECT queue_number FROM queue_allocations WHERE store_id = ?1 AND order_id = ?2",
            params![store, order],
            |row| row.get(0),
        )
        .optional()
        .map_err(|error| db_error(port, error))?;

    let number = if let Some(number) = existing {
        number
    } else {
        // Ensure a counter row for this (store, date) starting at 1, read the number it will hand
        // out, advance it, and record the allocation — all inside this transaction, so a rollback
        // consumes nothing and a new business date begins its own sequence at 1.
        tx.execute(
            "INSERT INTO queue_counter (store_id, business_date, next_number) VALUES (?1, ?2, 1)
             ON CONFLICT (store_id, business_date) DO NOTHING",
            params![store, date],
        )
        .map_err(|error| db_error(port, error))?;
        let allocated: i64 = tx
            .query_row(
                "SELECT next_number FROM queue_counter WHERE store_id = ?1 AND business_date = ?2",
                params![store, date],
                |row| row.get(0),
            )
            .map_err(|error| db_error(port, error))?;
        tx.execute(
            "UPDATE queue_counter SET next_number = next_number + 1
             WHERE store_id = ?1 AND business_date = ?2",
            params![store, date],
        )
        .map_err(|error| db_error(port, error))?;
        tx.execute(
            "INSERT INTO queue_allocations (store_id, order_id, queue_number) VALUES (?1, ?2, ?3)",
            params![store, order, allocated],
        )
        .map_err(|error| db_error(port, error))?;
        allocated
    };

    tx.commit().map_err(|error| db_error(port, error))?;
    Ok(u64::try_from(number).unwrap_or(0))
}

/// The stored `IntakeRecord` JSON for a `(store, sales_channel, external_reference)`, or `None`
/// (ADR-0064). The store layer deserialises; the writer stays free of `pos_ports` types.
fn look_up_intake(
    conn: &Connection,
    store_id: StoreId,
    sales_channel: &str,
    external_reference: &str,
) -> Result<Option<String>, PortError> {
    let port = PortName::IntakeLedger;
    conn.query_row(
        "SELECT record FROM intake_ledger
         WHERE store_id = ?1 AND sales_channel = ?2 AND external_reference = ?3",
        params![store_id.to_string(), sales_channel, external_reference],
        |row| row.get::<_, String>(0),
    )
    .optional()
    .map_err(|error| db_error(port, error))
}

// -----------------------------------------------------------------------------------------------
// The device registry (ADR-0091). Every function here is unconditional-write or plain read: the
// idle timeout is the caller's rule, so nothing below knows it exists.
// -----------------------------------------------------------------------------------------------

/// The port these functions report failures under, so a registry fault is not attributed to
/// whichever port happens to sit nearby in a metric label.
const REGISTRY: PortName = PortName::DeviceRegistry;

fn record_pairing(conn: &Connection, device: &PairedDeviceRow) -> Result<(), PortError> {
    // Replace on either key. `device_id` is the primary key, and `token_digest` is UNIQUE — a
    // re-pair of the same device mints a fresh token, so the old digest row has to go or the
    // UNIQUE constraint refuses the insert. Deleting by digest first also covers the (impossible
    // in practice, 128 bits) case of a digest arriving for a different device.
    conn.execute(
        "DELETE FROM paired_devices WHERE token_digest = ?1",
        params![&device.token_digest],
    )
    .map_err(|error| db_error(REGISTRY, error))?;
    conn.execute(
        "INSERT INTO paired_devices (device_id, token_digest, paired_at_ms)
         VALUES (?1, ?2, ?3)
         ON CONFLICT (device_id) DO UPDATE SET
             token_digest = excluded.token_digest,
             paired_at_ms = excluded.paired_at_ms",
        params![&device.device_id, &device.token_digest, device.paired_at_ms],
    )
    .map(|_| ())
    .map_err(|error| db_error(REGISTRY, error))
}

fn device_for_digest(conn: &Connection, token_digest: &str) -> Result<Option<String>, PortError> {
    conn.query_row(
        "SELECT device_id FROM paired_devices WHERE token_digest = ?1",
        params![token_digest],
        |row| row.get::<_, String>(0),
    )
    .optional()
    .map_err(|error| db_error(REGISTRY, error))
}

fn paired_devices(conn: &Connection) -> Result<Vec<PairedDeviceRow>, PortError> {
    let mut statement = conn
        .prepare("SELECT device_id, token_digest, paired_at_ms FROM paired_devices")
        .map_err(|error| db_error(REGISTRY, error))?;
    let rows = statement
        .query_map([], |row| {
            Ok(PairedDeviceRow {
                device_id: row.get(0)?,
                token_digest: row.get(1)?,
                paired_at_ms: row.get(2)?,
            })
        })
        .map_err(|error| db_error(REGISTRY, error))?;
    rows.collect::<rusqlite::Result<Vec<_>>>()
        .map_err(|error| db_error(REGISTRY, error))
}

/// Retires one device, or every device when `device_id` is `None`.
///
/// Both tables, in one transaction. `ON DELETE CASCADE` in migration 0005 says the same thing, and
/// the session delete is issued explicitly anyway: a session belonging to no paired device is
/// unreachable state a later feature could read as live, and that must not depend on a `PRAGMA`
/// being set. The transaction is what makes the pair atomic — a crash between them would leave
/// exactly the disagreement this guards against.
fn revoke_devices(conn: &mut Connection, device_id: Option<&str>) -> Result<(), PortError> {
    let tx = conn
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(|error| db_error(REGISTRY, error))?;
    // `?1 IS NULL OR device_id = ?1` makes revoke-all the *same* statement as revoke-one, so there
    // is one code path rather than two that could drift — and the break-glass cannot end up
    // clearing one table while the single-device case clears both.
    for sql in [
        "DELETE FROM device_sessions WHERE ?1 IS NULL OR device_id = ?1",
        "DELETE FROM paired_devices WHERE ?1 IS NULL OR device_id = ?1",
    ] {
        tx.execute(sql, params![device_id])
            .map_err(|error| db_error(REGISTRY, error))?;
    }
    tx.commit().map_err(|error| db_error(REGISTRY, error))
}

fn record_sign_in(conn: &Connection, session: &DeviceSessionRow) -> Result<(), PortError> {
    conn.execute(
        "INSERT INTO device_sessions
             (device_id, employee_id, signed_in_at_ms, last_seen_at_ms)
         VALUES (?1, ?2, ?3, ?4)
         ON CONFLICT (device_id) DO UPDATE SET
             employee_id     = excluded.employee_id,
             signed_in_at_ms = excluded.signed_in_at_ms,
             last_seen_at_ms = excluded.last_seen_at_ms",
        params![
            &session.device_id,
            &session.employee_id,
            session.signed_in_at_ms,
            session.last_seen_at_ms
        ],
    )
    .map(|_| ())
    .map_err(|error| db_error(REGISTRY, error))
}

fn sign_in_for(conn: &Connection, device_id: &str) -> Result<Option<DeviceSessionRow>, PortError> {
    conn.query_row(
        "SELECT device_id, employee_id, signed_in_at_ms, last_seen_at_ms
         FROM device_sessions WHERE device_id = ?1",
        params![device_id],
        |row| {
            Ok(DeviceSessionRow {
                device_id: row.get(0)?,
                employee_id: row.get(1)?,
                signed_in_at_ms: row.get(2)?,
                last_seen_at_ms: row.get(3)?,
            })
        },
    )
    .optional()
    .map_err(|error| db_error(REGISTRY, error))
}

fn sign_ins(conn: &Connection) -> Result<Vec<DeviceSessionRow>, PortError> {
    let mut statement = conn
        .prepare(
            "SELECT device_id, employee_id, signed_in_at_ms, last_seen_at_ms FROM device_sessions",
        )
        .map_err(|error| db_error(REGISTRY, error))?;
    let rows = statement
        .query_map([], |row| {
            Ok(DeviceSessionRow {
                device_id: row.get(0)?,
                employee_id: row.get(1)?,
                signed_in_at_ms: row.get(2)?,
                last_seen_at_ms: row.get(3)?,
            })
        })
        .map_err(|error| db_error(REGISTRY, error))?;
    rows.collect::<rusqlite::Result<Vec<_>>>()
        .map_err(|error| db_error(REGISTRY, error))
}

/// Moves `last_seen_at` forward on an existing session only.
///
/// An `UPDATE` that matches no row affects nothing and reports success, which is exactly the
/// contract: the gate touches on every request, including one racing a sign-out, and a touch must
/// never *create* a session — that would make touching a way to sign a device in.
fn touch_session(conn: &Connection, device_id: &str, now_ms: i64) -> Result<(), PortError> {
    conn.execute(
        "UPDATE device_sessions SET last_seen_at_ms = ?2 WHERE device_id = ?1",
        params![device_id, now_ms],
    )
    .map(|_| ())
    .map_err(|error| db_error(REGISTRY, error))
}

fn clear_sign_in(conn: &Connection, device_id: &str) -> Result<(), PortError> {
    conn.execute(
        "DELETE FROM device_sessions WHERE device_id = ?1",
        params![device_id],
    )
    .map(|_| ())
    .map_err(|error| db_error(REGISTRY, error))
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
