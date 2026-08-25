// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! Daily takeaway queue-number allocation (ADR-0064, the edge `OrderIn` authority).
//!
//! Like the receipt counter, allocation is not a port — it is the edge adapter's concern — so it is
//! proven here directly rather than in the shared contract suite. The properties that matter in the
//! field are three: within a trading day the numbers are distinct even when two channels deliver at
//! once; a new business date restarts at 1 with no midnight job; and — the reason this counter is
//! durable rather than in-memory — the sequence **survives a restart**, so a box that loses power
//! mid-service does not shout `#1` at a second customer.

// The whole file is test scaffolding; a failed temp dir or runtime is an unrecoverable setup fault.
#![allow(
    clippy::expect_used,
    reason = "test scaffolding: a failed temp dir, runtime, or writer reply is an unrecoverable fault"
)]

use std::collections::BTreeSet;
use std::future::Future;
use std::path::{Path, PathBuf};

use pos_proto::ids::{OrderId, StoreId};
use pos_proto::time::BusinessDate;
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

fn order(n: u128) -> OrderId {
    OrderId::new(Ulid::from_u128(n))
}

fn day(d: u8) -> BusinessDate {
    BusinessDate::from_ymd(2026, 8, d).expect("a real date")
}

/// Two channels delivering at once, at scale: the concurrent-allocation load within one trading day.
const ORDERS: u128 = 200;

#[test]
fn concurrent_allocation_within_a_day_is_distinct_and_gapless() {
    let dir = TempDir::new().expect("temp dir");
    let path = dir.path().join("store.sqlite");
    let store = open(&path);
    let store_id = store_id();
    let date = day(24);

    let numbers = block_on(async move {
        // Submit every allocation before draining, so the commands genuinely queue at the writer.
        let mut handles = Vec::new();
        for index in 1..=ORDERS {
            let store = store.clone();
            handles.push(tokio::spawn(async move {
                store
                    .allocate_daily_queue_number(store_id, date, order(index))
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

    // Exactly 1..=ORDERS, each once: no gap (every number present) and no collision (no duplicate).
    let unique: BTreeSet<u64> = numbers.iter().copied().collect();
    assert_eq!(unique.len(), numbers.len(), "no two orders share a number");
    let expected: BTreeSet<u64> = (1..=u64::try_from(ORDERS).expect("fits")).collect();
    assert_eq!(unique, expected, "the sequence is 1..=N with no gaps");
}

#[test]
fn allocation_is_idempotent_per_order() {
    let dir = TempDir::new().expect("temp dir");
    let path = dir.path().join("store.sqlite");
    let store = open(&path);
    let store_id = store_id();
    let date = day(24);

    block_on(async move {
        let first = store
            .allocate_daily_queue_number(store_id, date, order(1))
            .await
            .expect("first");
        let again = store
            .allocate_daily_queue_number(store_id, date, order(1))
            .await
            .expect("again");
        assert_eq!(first, again, "the same order keeps its number");

        let other = store
            .allocate_daily_queue_number(store_id, date, order(2))
            .await
            .expect("other");
        assert_eq!(other, first + 1, "the counter advanced once, not twice");
    });
}

#[test]
fn a_new_business_date_restarts_at_one() {
    let dir = TempDir::new().expect("temp dir");
    let path = dir.path().join("store.sqlite");
    let store = open(&path);
    let store_id = store_id();

    block_on(async move {
        assert_eq!(
            store
                .allocate_daily_queue_number(store_id, day(24), order(1))
                .await
                .expect("d24 #1"),
            1
        );
        assert_eq!(
            store
                .allocate_daily_queue_number(store_id, day(24), order(2))
                .await
                .expect("d24 #2"),
            2
        );
        // A new trading day the counter has never seen starts its own sequence at 1 — the daily
        // reset, with no midnight job.
        assert_eq!(
            store
                .allocate_daily_queue_number(store_id, day(25), order(3))
                .await
                .expect("d25 #1"),
            1,
            "a new business date restarts at one"
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
                .allocate_daily_queue_number(store_id, day(24), order(1))
                .await
                .expect("first");
            let second = store
                .allocate_daily_queue_number(store_id, day(24), order(2))
                .await
                .expect("second");
            (first, second)
        }
    });
    assert_eq!((first, second), (1, 2));

    // Reopen the same file (power was pulled mid-service): the same trading day CONTINUES rather
    // than resetting — the reason this counter is durable and not in-memory — while a genuinely new
    // day still restarts at 1, and an order that already had a number keeps it.
    block_on(async move {
        let store = open(&path);
        let third = store
            .allocate_daily_queue_number(store_id, day(24), order(3))
            .await
            .expect("third");
        assert_eq!(
            third, 3,
            "the same day continues past a reopen — no reissued #1"
        );

        let order_one_again = store
            .allocate_daily_queue_number(store_id, day(24), order(1))
            .await
            .expect("order one again");
        assert_eq!(order_one_again, 1, "an allocation survives a reopen");

        let new_day = store
            .allocate_daily_queue_number(store_id, day(25), order(4))
            .await
            .expect("new day");
        assert_eq!(
            new_day, 1,
            "a new business date still restarts at one after a reopen"
        );
    });
}
