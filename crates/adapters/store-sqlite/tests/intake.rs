// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The durable inbound-order idempotency ledger (ADR-0064, the edge `OrderIn` authority).
//!
//! The ledger is what makes a retry — or the relay's at-least-once redelivery, or a crash mid-open —
//! converge on one order instead of many. Its two load-bearing properties are proven here directly:
//! a recorded key **survives a restart** (so dedup works after the box loses power), and a second
//! order racing in on the **same key is refused, not duplicated** (the plain insert's constraint
//! violation, surfaced as `already_exists`, which the one writer thread serialises cleanly). The
//! atomic-with-the-order commit itself is structural — the row is written in the order's own SQLite
//! transaction — and is exercised end-to-end by the shared `OrderIn` contract suite.

// The whole file is test scaffolding; a failed temp dir or runtime is an unrecoverable setup fault.
#![allow(
    clippy::expect_used,
    reason = "test scaffolding: a failed temp dir, runtime, or writer reply is an unrecoverable fault"
)]

use std::future::Future;
use std::path::{Path, PathBuf};

use pos_ports::intake_ledger::{IntakeLedger, IntakeRecord};
use pos_ports::{PortError, Transactional, TxContext};
use pos_proto::error::ErrorStatus;
use pos_proto::ids::{OrderId, StoreId};
use pos_proto::money::{CurrencyCode, Money};
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

fn record(order: u128) -> IntakeRecord {
    IntakeRecord {
        order_id: OrderId::new(Ulid::from_u128(order)),
        business_date: BusinessDate::from_ymd(2026, 8, 24).expect("a real date"),
        total: Money::new(CurrencyCode::VND, 120_000),
        repriced: false,
        awaiting_staff_confirmation: false,
    }
}

/// Records a key in its own transaction, exactly as `open_inbound_order` does alongside the events.
async fn record_key(
    store: &SqliteStore,
    channel: &str,
    reference: &str,
    entry: &IntakeRecord,
) -> Result<(), PortError> {
    let mut tx = store.begin().await?;
    store
        .record(&mut tx, store_id(), channel, reference, entry)
        .await?;
    tx.commit().await
}

#[test]
fn a_recorded_key_can_be_looked_up() {
    let dir = TempDir::new().expect("temp dir");
    let path = dir.path().join("store.sqlite");
    let store = open(&path);

    block_on(async move {
        let entry = record(1);
        record_key(&store, "grab_food", "grab-abc", &entry)
            .await
            .expect("record");
        let found = store
            .look_up(store_id(), "grab_food", "grab-abc")
            .await
            .expect("look up");
        assert_eq!(found.as_ref(), Some(&entry), "the record round-trips");

        // A key never recorded is absent, and the channel scopes the key.
        assert!(
            store
                .look_up(store_id(), "grab_food", "never-seen")
                .await
                .expect("look up")
                .is_none()
        );
        assert!(
            store
                .look_up(store_id(), "shopee_food", "grab-abc")
                .await
                .expect("look up")
                .is_none(),
            "the same reference on another channel is a different key"
        );
    });
}

#[test]
fn a_recorded_key_survives_reopening_the_database() {
    let dir = TempDir::new().expect("temp dir");
    let path: PathBuf = dir.path().join("store.sqlite");
    let entry = record(7);

    block_on({
        let path = path.clone();
        let entry = entry.clone();
        async move {
            let store = open(&path);
            record_key(&store, "grab_food", "grab-xyz", &entry)
                .await
                .expect("record");
        }
    });

    // Reopen the same file (power was pulled): the record is still there, so a redelivery after a
    // restart dedupes rather than opening a second order — the reason the ledger is durable.
    block_on(async move {
        let store = open(&path);
        let found = store
            .look_up(store_id(), "grab_food", "grab-xyz")
            .await
            .expect("look up");
        assert_eq!(found.as_ref(), Some(&entry), "the record survives a reopen");
    });
}

#[test]
fn a_duplicate_key_is_refused_and_the_first_record_stands() {
    let dir = TempDir::new().expect("temp dir");
    let path = dir.path().join("store.sqlite");
    let store = open(&path);

    block_on(async move {
        let first = record(1);
        record_key(&store, "grab_food", "same-ref", &first)
            .await
            .expect("first record");

        // A second order racing in on the same key must be refused, not duplicated — the plain
        // insert's constraint violation, surfaced as `already_exists`.
        let second = record(2);
        let conflict = record_key(&store, "grab_food", "same-ref", &second)
            .await
            .expect_err("the duplicate key is refused");
        assert_eq!(
            conflict.status(),
            ErrorStatus::AlreadyExists,
            "a repeat key is already_exists, which the caller resolves by looking up the winner"
        );

        // The first record stands untouched — the loser changed nothing.
        let found = store
            .look_up(store_id(), "grab_food", "same-ref")
            .await
            .expect("look up");
        assert_eq!(found.as_ref(), Some(&first), "the first writer wins");
    });
}
