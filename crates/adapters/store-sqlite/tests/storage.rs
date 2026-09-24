// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! How a store lays out its file (finding F5).
//!
//! An event row is just over the ~1,000 bytes a 4 KiB page keeps locally for a WITHOUT ROWID table,
//! so at the default page size nearly every event took an overflow page of its own — about 4.7 KB on
//! disk for 0.9 KB of data. A new store's file is created with 8 KiB pages, which keeps the whole row
//! on the page. And the unchained-row count the chain walk makes is answered from a partial index
//! rather than a read of the whole log.

#![allow(
    clippy::expect_used,
    reason = "test scaffolding: a failed temp dir or open is an unrecoverable setup fault"
)]

use store_sqlite::SqliteStore;

#[test]
fn a_new_store_is_created_with_pages_an_event_fits_on() {
    let dir = tempfile::tempdir().expect("temp dir");
    let path = dir.path().join("store.db");
    drop(SqliteStore::open(&path).expect("open"));

    let file = rusqlite::Connection::open(&path).expect("open the file");
    let page_size: i64 = file
        .query_row("PRAGMA page_size", [], |row| row.get(0))
        .expect("read the page size");
    assert_eq!(page_size, 8192);
}

#[test]
fn the_unchained_events_have_an_index_to_be_counted_from() {
    let dir = tempfile::tempdir().expect("temp dir");
    let path = dir.path().join("store.db");
    drop(SqliteStore::open(&path).expect("open"));

    // The writer counts through it with `INDEXED BY`, which is an error — not a slow fallback —
    // if the index is missing, so every chain test is also a test that this exists. Named here so
    // the reason it exists is findable from the schema side too.
    let file = rusqlite::Connection::open(&path).expect("open the file");
    let partial: String = file
        .query_row(
            "SELECT sql FROM sqlite_master WHERE type = 'index' AND name = 'idx_events_store_id_unchained'",
            [],
            |row| row.get(0),
        )
        .expect("the index exists");
    assert!(
        partial.contains("WHERE seq IS NULL"),
        "partial on unchained rows: {partial}"
    );
}
