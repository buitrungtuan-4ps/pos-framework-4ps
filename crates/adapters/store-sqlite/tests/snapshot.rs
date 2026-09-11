// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! `VACUUM INTO`: a copy of a live store that is a store (ADR-0124).
//!
//! The failure this file exists to catch is the quiet one. A store is WAL, so the committed truth
//! at any instant is spread across `store.sqlite` and its `-wal` sidecar; a snapshot taken the
//! obvious way — copy the file — yields a database that opens, reads, and is missing every sale
//! since the last checkpoint. Nothing about it looks wrong until somebody restores it. So the
//! assertions here are about *content*: the same receipt numbers, the same allocations, from a
//! snapshot taken while the writer was busy.

// The whole file is test scaffolding; a failed temp dir or runtime is an unrecoverable setup fault.
#![allow(
    clippy::expect_used,
    reason = "test scaffolding: a failed temp dir, runtime, or writer reply is an unrecoverable fault"
)]

use std::future::Future;
use std::path::Path;

use pos_proto::ids::{BillId, StoreId};
use pos_proto::ulid::Ulid;
use store_sqlite::{SqliteStore, snapshot_to};
use tempfile::TempDir;

fn block_on<F: Future>(future: F) -> F::Output {
    tokio::runtime::Builder::new_current_thread()
        .build()
        .expect("build a current-thread tokio runtime")
        .block_on(future)
}

fn store_id() -> StoreId {
    StoreId::new(Ulid::from_u128(0xB00))
}

/// Reads back what a snapshot holds, using SQLite directly — a restore is somebody opening this
/// file, so the test opens it the same way rather than through the adapter that wrote it.
fn receipts_in(path: &Path) -> Vec<i64> {
    let connection = rusqlite::Connection::open(path).expect("open the snapshot");
    let mut statement = connection
        .prepare("SELECT receipt_number FROM receipt_allocations ORDER BY receipt_number")
        .expect("prepare");
    statement
        .query_map([], |row| row.get::<_, i64>(0))
        .expect("query")
        .collect::<Result<Vec<_>, _>>()
        .expect("rows")
}

#[test]
fn a_snapshot_of_a_live_store_carries_everything_committed_to_it() {
    let dir = TempDir::new().expect("temp dir");
    let live = dir.path().join("store.sqlite");
    let copy = dir.path().join("snapshot.sqlite");

    let allocated = block_on(async {
        let store = SqliteStore::open(&live).expect("open the store");
        let mut numbers = Vec::new();
        for bill in 1..=50_u128 {
            numbers.push(
                store
                    .allocate_receipt_number(store_id(), BillId::new(Ulid::from_u128(bill)))
                    .await
                    .expect("allocate"),
            );
        }
        // Deliberately do NOT close the store: the point is a snapshot of a *running* till, with
        // the writer thread alive and the WAL un-checkpointed.
        let size = snapshot_to(&live, &copy).expect("snapshot");
        assert!(size > 0, "a snapshot of a real store is not empty");
        drop(store);
        numbers
    });

    assert_eq!(
        receipts_in(&copy),
        allocated
            .iter()
            .map(|number| i64::try_from(*number).expect("fits"))
            .collect::<Vec<_>>(),
        "every receipt number the live store issued is in the snapshot — the WAL was not left \
         behind"
    );
}

#[test]
fn a_snapshot_taken_while_the_writer_is_busy_is_still_a_whole_database() {
    let dir = TempDir::new().expect("temp dir");
    let live = dir.path().join("store.sqlite");
    let copy = dir.path().join("snapshot.sqlite");

    let issued = block_on(async {
        let store = SqliteStore::open(&live).expect("open the store");
        // Queue a burst at the writer and take the snapshot without waiting for it. `VACUUM INTO`
        // is read-only with respect to the source, so this must neither fail nor block on the
        // write lock — and whatever it captures must be a consistent prefix, never a torn page.
        let busy = tokio::spawn({
            let store = store.clone();
            async move {
                let mut last = 0;
                for bill in 1..=400_u128 {
                    last = store
                        .allocate_receipt_number(store_id(), BillId::new(Ulid::from_u128(bill)))
                        .await
                        .expect("allocate");
                }
                last
            }
        });
        snapshot_to(&live, &copy).expect("snapshot while the writer is busy");
        let last = busy.await.expect("the burst finished");
        drop(store);
        last
    });

    let captured = receipts_in(&copy);
    assert!(
        captured.len() <= usize::try_from(issued).expect("fits"),
        "a snapshot cannot hold more than was ever allocated"
    );
    assert_eq!(
        captured,
        (1..=i64::try_from(captured.len()).expect("fits")).collect::<Vec<_>>(),
        "what the snapshot caught is a gapless prefix — a consistent point in time, not a torn \
         read"
    );
}

#[test]
fn a_snapshot_will_not_overwrite_a_file_that_is_already_there() {
    let dir = TempDir::new().expect("temp dir");
    let live = dir.path().join("store.sqlite");
    let copy = dir.path().join("snapshot.sqlite");
    block_on(async {
        let store = SqliteStore::open(&live).expect("open the store");
        drop(store);
    });
    std::fs::write(&copy, b"somebody else's file").expect("write");

    let refused = snapshot_to(&live, &copy);

    assert!(
        refused.is_err(),
        "vacuuming into an existing file is refused, not merged into it"
    );
    assert_eq!(
        std::fs::read(&copy).expect("read"),
        b"somebody else's file",
        "and the file that was there is untouched"
    );
}
