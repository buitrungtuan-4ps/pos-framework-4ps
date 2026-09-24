// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! Rebuilding a store database created before 8 KiB pages (finding F5).
//!
//! A new store's file is written at 8 KiB pages, where an event row keeps its envelope on the page
//! instead of spilling into an overflow page of its own; the same log is about a quarter of the
//! size. But SQLite ignores `PRAGMA page_size` on a file that already has pages, so every store
//! created before that change kept its 4 KiB pages, and its database stayed four times the size it
//! needs to be. [`rebuild_page_size`] fixes that once: `VACUUM` rewrites the file at the new size.
//!
//! `VACUUM` cannot change the page size of a database in WAL mode, so the rebuild steps out of WAL
//! for its duration and back in after. That needs the only connection to the file, so it runs
//! **before** [`SqliteStore::open`](crate::SqliteStore::open) starts the writer thread, and never
//! while anything else has the file open: a busy file refuses, and is left exactly as it was. A
//! rebuild interrupted by a crash or a full disk is rolled back by SQLite's own journal; the store
//! then opens on its old page size and tries again next boot.

use std::path::Path;

use rusqlite::{Connection, OpenFlags};

use pos_ports::{PortError, PortName};

/// The page size a store database is written at.
pub const PAGE_SIZE: u32 = 8192;

/// What [`rebuild_page_size`] found or did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PageRebuild {
    /// There is no database yet, or it is already at [`PAGE_SIZE`].
    NotNeeded,
    /// The database was rewritten at [`PAGE_SIZE`].
    Rebuilt {
        /// The page size it had.
        from: u32,
        /// How many pages it had at that size.
        pages: u64,
    },
}

/// Whether the database at `path` exists and is at a page size other than [`PAGE_SIZE`], for a
/// caller that wants to say a rebuild is coming before it starts one.
///
/// # Errors
///
/// [`PortError::unavailable`] if the file exists and cannot be read as a database.
pub fn needs_page_rebuild(path: &Path) -> Result<bool, PortError> {
    Ok(current(path)?.is_some_and(|(size, _)| size != PAGE_SIZE))
}

/// Rewrites the database at `path` at [`PAGE_SIZE`] if it is at any other size. Call it before
/// opening the store, never while the file is open elsewhere.
///
/// # Errors
///
/// [`PortError::unavailable`] if the file cannot be read, is busy, or the rewrite fails. The
/// database is then exactly as it was, and still opens.
pub fn rebuild_page_size(path: &Path) -> Result<PageRebuild, PortError> {
    let Some((from, pages)) = current(path)? else {
        return Ok(PageRebuild::NotNeeded);
    };
    if from == PAGE_SIZE {
        return Ok(PageRebuild::NotNeeded);
    }
    let connection = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_WRITE)
        .map_err(|error| failed("could not open the database to rebuild it", error))?;
    connection
        .execute_batch(&format!(
            "PRAGMA journal_mode = DELETE;
             PRAGMA page_size = {PAGE_SIZE};
             VACUUM;
             PRAGMA journal_mode = WAL;"
        ))
        .map_err(|error| failed("could not rebuild the database at the new page size", error))?;
    Ok(PageRebuild::Rebuilt { from, pages })
}

/// The file's page size and page count, or `None` when there is no database there yet.
fn current(path: &Path) -> Result<Option<(u32, u64)>, PortError> {
    if !path.exists() {
        return Ok(None);
    }
    let connection = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)
        .map_err(|error| failed("could not open the database to read its page size", error))?;
    let read = |pragma: &str| -> Result<i64, PortError> {
        connection
            .query_row(pragma, [], |row| row.get(0))
            .map_err(|error| failed("could not read the database's page size", error))
    };
    let size = u32::try_from(read("PRAGMA page_size")?).unwrap_or(PAGE_SIZE);
    let pages = u64::try_from(read("PRAGMA page_count")?).unwrap_or(0);
    // An empty file has no pages to rewrite: opening it sets the page size before the first one.
    Ok((pages > 0).then_some((size, pages)))
}

fn failed(message: &'static str, error: rusqlite::Error) -> PortError {
    PortError::unavailable(PortName::EventStore, message).with_source(error)
}
