// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The store's OTA self-test, across the restart an install performs (migration 0006, ADR-0048's
//! rollback rule).
//!
//! Like the receipt and queue counters, this is edge-local durable state rather than a port, so it
//! is proven here directly instead of in the shared contract suite. One property is the entire
//! reason the table exists: the verdict **survives a restart**. `decide_rollout` puts the rollback
//! rule above every other, above even the kill switch — and an install deliberately reboots the box
//! (ADR-0055). While the verdict lived in process memory, a box that installed a bad build, failed
//! its self-test and restarted came back with no memory of failing and was eligible to install the
//! same bad build again. That loop is what these tests close.

// The whole file is test scaffolding; a failed temp dir or runtime is an unrecoverable setup fault.
#![allow(
    clippy::expect_used,
    reason = "test scaffolding: a failed temp dir, runtime, or writer reply is an unrecoverable fault"
)]

use std::future::Future;
use std::path::Path;

use pos_proto::ids::StoreId;
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
    StoreId::new(Ulid::from_u128(0x00C0_FFEE))
}

#[test]
fn a_self_test_survives_the_restart_an_install_performs() {
    let dir = TempDir::new().expect("temp dir");
    let path = dir.path().join("store.sqlite");

    block_on(async {
        let store = open(&path);
        assert_eq!(
            store.last_ota_self_test(store_id()).await.expect("read"),
            None,
            "a box that has never installed anything has nothing to revert from"
        );
        // The box installs 1.4.0 and its self-test fails.
        store
            .record_ota_self_test(store_id(), "1.4.0".to_owned(), false)
            .await
            .expect("record the failure");
    });

    // The install reboots the box. This reopen is that reboot — and the verdict is still there,
    // which is the whole point: the rollback rule can now fire from real state.
    block_on(async {
        let store = open(&path);
        assert_eq!(
            store.last_ota_self_test(store_id()).await.expect("read"),
            Some(("1.4.0".to_owned(), false)),
            "the failed self-test survived the restart it was recorded before"
        );
    });
}

#[test]
fn the_latest_verdict_replaces_the_earlier_one_and_is_kept_per_store() {
    let dir = TempDir::new().expect("temp dir");
    let path = dir.path().join("store.sqlite");
    let store = open(&path);
    let other = StoreId::new(Ulid::from_u128(0xBEEF));

    block_on(async {
        store
            .record_ota_self_test(store_id(), "1.4.0".to_owned(), false)
            .await
            .expect("record");
        // A later self-test replaces the earlier verdict rather than adding to a history the
        // decision would then have to choose between: an upsert, not an append.
        store
            .record_ota_self_test(store_id(), "1.4.1".to_owned(), true)
            .await
            .expect("record again");
        assert_eq!(
            store.last_ota_self_test(store_id()).await.expect("read"),
            Some(("1.4.1".to_owned(), true))
        );

        // One store's verdict is not another's — a shared row would let one box's failure revert a
        // fleet.
        assert_eq!(store.last_ota_self_test(other).await.expect("read"), None);
        store
            .record_ota_self_test(other, "1.3.0".to_owned(), true)
            .await
            .expect("record for the other store");
        assert_eq!(
            store.last_ota_self_test(store_id()).await.expect("read"),
            Some(("1.4.1".to_owned(), true)),
            "writing another store's verdict left this one alone"
        );
    });
}
