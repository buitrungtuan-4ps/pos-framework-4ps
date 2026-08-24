// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The migration runner (ADR-0017): forward-only, numbered SQL files, applied in order.
//!
//! A loop, not a framework. Each file whose number is greater than the database's recorded version
//! runs in its own transaction and is recorded in `schema_migrations`. A file that fails leaves the
//! version one behind, so the next boot resumes at the first unapplied file — the multi-version jump
//! a store that was offline for weeks makes is just more iterations.
//!
//! The files themselves are immutable once merged; `cargo xtask migrations` refuses an edit to a
//! shipped file or a destructive statement. Adding a schema change is a new numbered file, never an
//! edit here.

use rusqlite::Connection;

/// Every migration, in order, embedded at build time. A new schema change appends a `(version, sql)`
/// pair pointing at a new file — the existing entries are never edited.
const MIGRATIONS: &[(u32, &str)] = &[
    (1, include_str!("../migrations/0001_event_store.sql")),
    (2, include_str!("../migrations/0002_receipt_counter.sql")),
    (3, include_str!("../migrations/0003_queue_counter.sql")),
];

/// Applies every migration the database has not yet seen.
///
/// # Errors
///
/// Any `rusqlite` error from creating the ledger table, reading the current version, or applying a
/// migration.
pub(crate) fn run(conn: &mut Connection) -> rusqlite::Result<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS schema_migrations (
            version      INTEGER PRIMARY KEY,
            applied_time TEXT NOT NULL
        );",
    )?;

    let current: u32 = conn.query_row(
        "SELECT COALESCE(MAX(version), 0) FROM schema_migrations",
        [],
        |row| row.get(0),
    )?;

    for &(version, sql) in MIGRATIONS {
        if version > current {
            let tx = conn.transaction()?;
            tx.execute_batch(sql)?;
            tx.execute(
                "INSERT INTO schema_migrations (version, applied_time) VALUES (?1, datetime('now'))",
                rusqlite::params![version],
            )?;
            tx.commit()?;
        }
    }
    Ok(())
}
