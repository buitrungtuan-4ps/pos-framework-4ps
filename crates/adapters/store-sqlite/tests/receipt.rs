// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! Gapless receipt-number allocation (ADR-0025, the `store_server` authority).
//!
//! Allocation is not a port — which authority mints the number is the edge adapter's concern — so it
//! is not part of the shared contract suite; it is proven here directly. The load the store server
//! actually sees is two cashier devices settling at once, which in this design is many concurrent
//! `allocate_receipt_number` calls funnelling through one writer thread. The test drives exactly that
//! and asserts the result is a gapless, collision-free sequence.

// The whole file is test scaffolding; a failed temp dir or runtime is an unrecoverable setup fault.
#![allow(
    clippy::expect_used,
    reason = "test scaffolding: a failed temp dir, runtime, or writer reply is an unrecoverable fault"
)]

use std::collections::BTreeSet;
use std::future::Future;
use std::path::{Path, PathBuf};

use pos_proto::ids::{BillId, StoreId};
use pos_proto::ulid::Ulid;
use store_sqlite::SqliteStore;
use tempfile::TempDir;

/// Drives a future on a fresh current-thread runtime — the executor a real edge binary supplies.
fn block_on<F: Future>(future: F) -> F::Output {
    tokio::runtime::Builder::new_current_thread()
        .build()
        .expect("build a current-thread tokio runtime")
        .block_on(future)
}

fn open(path: &Path) -> SqliteStore {
    SqliteStore::open(path).expect("open the store")
}

fn store_id() -> StoreId {
    StoreId::new(Ulid::from_u128(0xB00))
}

fn bill(n: u128) -> BillId {
    BillId::new(Ulid::from_u128(n))
}

/// Two cashier devices at once, at scale: the concurrent-allocation load ADR-0025 is about.
const BILLS: u128 = 200;

#[test]
fn concurrent_allocation_is_gapless_and_collision_free() {
    let dir = TempDir::new().expect("temp dir");
    let path = dir.path().join("store.sqlite");
    let store = open(&path);
    let store_id = store_id();

    let numbers = block_on(async move {
        // Submit every allocation before draining, so the commands genuinely queue at the writer —
        // the two-devices-at-once load, at scale.
        let mut handles = Vec::new();
        for index in 1..=BILLS {
            let store = store.clone();
            handles.push(tokio::spawn(async move {
                store
                    .allocate_receipt_number(store_id, bill(index))
                    .await
                    .expect("allocate")
            }));
        }
        let mut numbers = Vec::new();
        for handle in handles {
            numbers.push(handle.await.expect("task joins"));
        }
        numbers
    });

    // Exactly 1..=BILLS, each once: no gap (every number present) and no collision (no duplicate).
    let unique: BTreeSet<u64> = numbers.iter().copied().collect();
    assert_eq!(unique.len(), numbers.len(), "no two bills share a number");
    let expected: BTreeSet<u64> = (1..=u64::try_from(BILLS).expect("fits")).collect();
    assert_eq!(unique, expected, "the sequence is 1..=N with no gaps");
}

#[test]
fn allocation_is_idempotent_per_bill() {
    let dir = TempDir::new().expect("temp dir");
    let path = dir.path().join("store.sqlite");
    let store = open(&path);
    let store_id = store_id();

    block_on(async move {
        let first = store
            .allocate_receipt_number(store_id, bill(1))
            .await
            .expect("first");
        let again = store
            .allocate_receipt_number(store_id, bill(1))
            .await
            .expect("again");
        assert_eq!(first, again, "the same bill keeps its number");

        // A different bill takes the next number — proof the counter advanced exactly once, not
        // twice, for the repeated bill.
        let other = store
            .allocate_receipt_number(store_id, bill(2))
            .await
            .expect("other");
        assert_eq!(other, first + 1, "the counter advanced once, not twice");
    });
}

#[test]
fn each_store_has_its_own_sequence() {
    let dir = TempDir::new().expect("temp dir");
    let path = dir.path().join("store.sqlite");
    let store = open(&path);
    let a = StoreId::new(Ulid::from_u128(0xA));
    let b = StoreId::new(Ulid::from_u128(0xB));

    block_on(async move {
        assert_eq!(
            store.allocate_receipt_number(a, bill(1)).await.expect("a1"),
            1
        );
        assert_eq!(
            store.allocate_receipt_number(b, bill(1)).await.expect("b1"),
            1
        );
        assert_eq!(
            store.allocate_receipt_number(a, bill(2)).await.expect("a2"),
            2
        );
        assert_eq!(
            store.allocate_receipt_number(b, bill(2)).await.expect("b2"),
            2
        );
    });
}

#[test]
fn the_counter_survives_reopening_the_database() {
    let dir = TempDir::new().expect("temp dir");
    let path: PathBuf = dir.path().join("store.sqlite");
    let store_id = store_id();

    let (first, second) = block_on({
        let path = path.clone();
        async move {
            let store = open(&path);
            let first = store
                .allocate_receipt_number(store_id, bill(1))
                .await
                .expect("first");
            let second = store
                .allocate_receipt_number(store_id, bill(2))
                .await
                .expect("second");
            (first, second)
        }
    });
    assert_eq!((first, second), (1, 2));

    // Reopen the same file (power was pulled): the counter continues rather than resetting, and an
    // already-allocated bill still resolves to its original number.
    block_on(async move {
        let store = open(&path);
        let third = store
            .allocate_receipt_number(store_id, bill(3))
            .await
            .expect("third");
        assert_eq!(third, 3, "the sequence continues past a reopen");

        let bill_one_again = store
            .allocate_receipt_number(store_id, bill(1))
            .await
            .expect("bill one again");
        assert_eq!(bill_one_again, 1, "an allocation survives a reopen");
    });
}
