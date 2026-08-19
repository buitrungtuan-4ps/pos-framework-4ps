// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! `store-sqlite` against the shared `EventStore` and `ConfigStore` contract suites.
//!
//! The same cases that run against the in-memory fake run here, unchanged — including
//! crash-mid-transaction, which the harness drives by reopening the database file without a clean
//! shutdown. If this adapter and the fake disagree, one of them is wrong, and the domain suite
//! (which runs on the fake) would be testing the wrong thing.
//!
//! Unlike the fake, these futures genuinely suspend — an `await` here waits for the writer thread —
//! so the suites run under a real `tokio` current-thread runtime rather than a one-poll executor.

// The whole file is test scaffolding. `allow-expect-in-tests` in clippy.toml scopes to `#[test]`
// and `#[cfg(test)]`, which does not reach an integration test's module-level helpers, so the
// harness setup (a temp dir, a runtime) is allowed to panic here explicitly.
#![allow(
    clippy::expect_used,
    reason = "test scaffolding: a failed temp dir or runtime is an unrecoverable test-setup fault"
)]

use std::future::Future;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

use pos_contract_tests::harness::{ConfigStoreHarness, EventStoreHarness, HarnessError, Setup};
use pos_proto::{StoreId, Ulid};
use store_sqlite::SqliteStore;
use tempfile::TempDir;

/// Drives a future to completion on a fresh current-thread runtime — the executor a real adapter
/// supplies to the suites (ADR-0026 §6).
fn block_on<F: Future>(future: F) -> F::Output {
    tokio::runtime::Builder::new_current_thread()
        .build()
        .expect("build a current-thread tokio runtime")
        .block_on(future)
}

/// A file-backed store in a temp directory. File-backed, not `:memory:`, because power-loss recovery
/// is only meaningful across a reopen of the same file.
struct StoreHarness {
    dir: TempDir,
    counter: AtomicU64,
}

impl StoreHarness {
    fn new() -> Self {
        Self {
            dir: tempfile::tempdir().expect("create a temp dir for the store"),
            counter: AtomicU64::new(0),
        }
    }

    /// A fresh database path within this harness's temp dir.
    fn next_path(&self) -> PathBuf {
        let n = self.counter.fetch_add(1, Ordering::Relaxed);
        self.dir.path().join(format!("store-{n}.sqlite"))
    }

    fn open(path: PathBuf) -> Setup<SqliteStore> {
        SqliteStore::open(path).map_err(|error| HarnessError::new(error.to_string()))
    }
}

impl EventStoreHarness for StoreHarness {
    type Store = SqliteStore;

    async fn fresh(&self) -> Setup<SqliteStore> {
        Self::open(self.next_path())
    }

    async fn lose_power(&self, store: SqliteStore) -> Setup<SqliteStore> {
        // Drop the store — no commit, no checkpoint, no clean shutdown — and reopen the same file.
        // Committed transactions are in the WAL and survive; a transaction still buffered in a
        // dropped `SqliteTx` never reached SQLite and is gone. That is the crash under test.
        let path = store.path().to_path_buf();
        drop(store);
        Self::open(path)
    }

    fn store_id(&self) -> StoreId {
        StoreId::new(Ulid::from_u128(0x0ADA))
    }
}

impl ConfigStoreHarness for StoreHarness {
    type Store = SqliteStore;

    async fn fresh(&self) -> Setup<SqliteStore> {
        Self::open(self.next_path())
    }

    async fn lose_power(&self, store: SqliteStore) -> Setup<SqliteStore> {
        let path = store.path().to_path_buf();
        drop(store);
        Self::open(path)
    }

    fn store_id(&self) -> StoreId {
        StoreId::new(Ulid::from_u128(0x0ADA))
    }
}

mod event_store {
    use super::{StoreHarness, block_on};
    pos_contract_tests::event_store_suite!(StoreHarness::new(), block_on);
}

mod config_store {
    use super::{StoreHarness, block_on};
    pos_contract_tests::config_store_suite!(StoreHarness::new(), block_on);
}
