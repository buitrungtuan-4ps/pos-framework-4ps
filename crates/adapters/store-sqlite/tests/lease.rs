// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The lease generation this box holds, taken once and kept across a restart (migration 0008,
//! [ADR-0108](../../../../docs/adr/0108-the-lease-generation-is-authority.md)).
//!
//! Edge-local durable state rather than a port, like the receipt and queue counters and the OTA
//! self-test, so it is proven here directly instead of in the shared contract suite. **Two**
//! properties are the reason the table exists, and both are here:
//!
//!  * **Take once.** A second take with a *different* generation must not overwrite the first. Make
//!    this an upsert and supersession is decorative: a machine a replacement took over pulls the
//!    next config, adopts the newer generation, and calls itself active again.
//!  * **Survives the restart.** An install deliberately reboots the box (ADR-0055). A held
//!    generation in process memory would be re-taken from config on every boot, which is the same
//!    bug with extra steps.

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
fn the_first_take_wins_and_a_later_generation_does_not_replace_it() {
    let dir = TempDir::new().expect("temp dir");
    let store = open(&dir.path().join("store.sqlite"));

    block_on(async {
        assert_eq!(
            store.held_lease(store_id()).await.expect("read"),
            None,
            "a box the cloud has never issued a lease to holds nothing"
        );

        // First sight: the box takes generation 4 and is the store.
        assert_eq!(store.take_lease(store_id(), 4).await.expect("take"), 4);
        assert_eq!(store.held_lease(store_id()).await.expect("read"), Some(4));

        // A replacement is activated and the cloud publishes generation 5. This box sees it on its
        // next config pull — and must **keep holding 4**, which is what makes it read `Superseded`.
        // If this returned 5, the machine would promote itself back and the lease would mean nothing.
        assert_eq!(
            store.take_lease(store_id(), 5).await.expect("take again"),
            4,
            "the take is `ON CONFLICT DO NOTHING`, not an upsert"
        );
        assert_eq!(store.held_lease(store_id()).await.expect("read"), Some(4));

        // Repeating the pull does not wear it down either.
        for _ in 0..5_u8 {
            assert_eq!(store.take_lease(store_id(), 5).await.expect("take"), 4);
        }

        // Nor does a *lower* generation, which is what a config rollback would publish. It leaves
        // the held value alone, so the box reads `Invalid` and refuses rather than being promoted.
        assert_eq!(store.take_lease(store_id(), 1).await.expect("take"), 4);
    });
}

#[test]
fn the_held_generation_survives_the_restart_an_install_performs() {
    let dir = TempDir::new().expect("temp dir");
    let path = dir.path().join("store.sqlite");

    block_on(async {
        let store = open(&path);
        store.take_lease(store_id(), 7).await.expect("take");
    });

    // The install reboots the box. This reopen is that reboot.
    block_on(async {
        let store = open(&path);
        assert_eq!(
            store.held_lease(store_id()).await.expect("read"),
            Some(7),
            "the generation this box holds outlives the restart"
        );
        // And it is still take-once on the other side of the reboot — the rule lives in the schema,
        // not in a process that just died.
        assert_eq!(store.take_lease(store_id(), 8).await.expect("take"), 7);
    });
}

#[test]
fn one_store_s_lease_is_not_another_s() {
    let dir = TempDir::new().expect("temp dir");
    let store = open(&dir.path().join("store.sqlite"));
    let other = StoreId::new(Ulid::from_u128(0xBEEF));

    block_on(async {
        store.take_lease(store_id(), 3).await.expect("take");
        assert_eq!(
            store.held_lease(other).await.expect("read"),
            None,
            "another store's first sight is still its own"
        );
        assert_eq!(store.take_lease(other, 9).await.expect("take"), 9);
        assert_eq!(
            store.held_lease(store_id()).await.expect("read"),
            Some(3),
            "taking one store's lease left the other alone"
        );
    });
}
