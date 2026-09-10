// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! A consistent copy of a live store, taken without stopping the shop
//! ([ADR-0124](../../../docs/adr/0124-a-store-that-can-be-restored.md)).
//!
//! # Why `VACUUM INTO` and not a file copy
//!
//! The database is WAL ([ADR-0015](../../../docs/adr/0015-sqlite-access.md)), so at any instant the
//! committed truth is spread across `store.sqlite` and `store.sqlite-wal`. Copying the main file
//! alone yields a database missing every commit since the last checkpoint; copying both with two
//! `cp`s yields a pair that disagree. `VACUUM INTO` asks SQLite itself for the answer: it reads the
//! source inside one read transaction and writes a fresh, fully checkpointed, defragmented database
//! to a new path.
//!
//! # Why it does not stall the till
//!
//! `VACUUM INTO` is documented as **read-only with respect to the source database**, and WAL lets a
//! reader run concurrently with the one writer. So the snapshot runs on its own connection, not on
//! the writer thread ([ADR-0015](../../../docs/adr/0015-sqlite-access.md) already permits extra
//! connections for reading), and a sale in progress is not waiting behind it.
//!
//! That connection is nevertheless opened **read-write**, because SQLite refuses `VACUUM INTO` on a
//! connection opened `SQLITE_OPEN_READ_ONLY` — the flag is a property of the connection, not of the
//! statement, and the check fires before SQLite notices that the only file being written is the new
//! one. Nothing here writes to the source; the `query_only` guarantee is bought by the statement
//! being the only one this connection ever runs.
//!
//! # Blocking
//!
//! This is a blocking call — SQLite's C API is synchronous, and a snapshot of a busy store is
//! seconds of work, not microseconds. It is deliberately **not** wrapped in an async method here:
//! this crate carries `tokio` with only the `sync` feature (no runtime), so the caller with a
//! runtime is the one that decides where the blocking happens. In `pos-edge` that is
//! `tokio::task::spawn_blocking`.

use std::path::Path;

use rusqlite::{Connection, OpenFlags};

use pos_ports::{PortError, PortName};

/// Writes a consistent copy of the database at `source` to `destination`, returning its size in
/// bytes.
///
/// `destination` must not exist: SQLite refuses to vacuum into a file that is already there, and
/// that refusal is worth keeping rather than papering over, because the alternative is a snapshot
/// silently merged into somebody else's file.
///
/// # Errors
///
/// [`PortError::unavailable`] if the source cannot be opened or the copy cannot be written —
/// including the case where `destination` already exists, which SQLite reports as an ordinary
/// error.
pub fn snapshot_to(source: &Path, destination: &Path) -> Result<u64, PortError> {
    // Read-write (see the module note) but never migrated: a snapshot must copy the schema the
    // running edge actually has, not quietly upgrade a database it does not own.
    let connection = Connection::open_with_flags(
        source,
        OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_URI,
    )
    .map_err(|error| snapshot_error("could not open the store to copy it", error))?;
    connection
        .busy_timeout(core::time::Duration::from_secs(30))
        .map_err(|error| snapshot_error("could not set the snapshot busy timeout", error))?;

    let target = destination.to_str().ok_or_else(|| {
        PortError::invalid_argument(
            PortName::EventStore,
            "the snapshot path is not valid UTF-8, and SQLite takes it as text",
        )
    })?;
    connection
        .execute("VACUUM INTO ?1", [target])
        .map_err(|error| snapshot_error("could not write the store snapshot", error))?;

    let size = std::fs::metadata(destination)
        .map_err(|error| {
            PortError::unavailable(
                PortName::EventStore,
                "the store snapshot could not be measured",
            )
            .with_source(error)
        })?
        .len();
    Ok(size)
}

fn snapshot_error(what: &'static str, error: rusqlite::Error) -> PortError {
    PortError::unavailable(PortName::EventStore, what).with_source(error)
}

/// Runs SQLite's own `PRAGMA integrity_check` over the database at `path` and returns what it
/// said — `"ok"` when the file is sound, otherwise the first problems it found.
///
/// This is the question a restore drill asks: not "did the bytes arrive" but "is what arrived a
/// database". It opens read-only and runs no migration, so it can be pointed at a restored copy
/// without changing it.
///
/// # Errors
///
/// [`PortError::unavailable`] if the file cannot be opened or the pragma cannot be run. A database
/// that opens and reports damage is `Ok` with the damage in the string — the caller decides what
/// a damaged restore means.
pub fn integrity_check(path: &Path) -> Result<String, PortError> {
    let connection = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_URI,
    )
    .map_err(|error| snapshot_error("could not open the database to check it", error))?;
    connection
        .query_row("PRAGMA integrity_check", [], |row| row.get::<_, String>(0))
        .map_err(|error| snapshot_error("the integrity check could not be run", error))
}
