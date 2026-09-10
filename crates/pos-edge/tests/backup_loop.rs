// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The backup loop, end to end against a real database
//! ([ADR-0124](../../docs/adr/0124-a-store-that-can-be-restored.md)).
//!
//! `store_archive.rs` proves the *format* — a sealed archive restores to a working store. This
//! proves the *loop*: that one round of [`BackupClient`] takes the key the cloud issued, snapshots
//! the live file the edge is actually using, seals it under that key, and hands the cloud
//! ciphertext. The cloud here is a stub that keeps whatever it is given, so the assertions can be
//! the two that matter — what reached the cloud is not the database, and what reached the cloud
//! opens back into one.

// The whole file is test scaffolding; a failed temp dir or runtime is an unrecoverable setup fault.
#![allow(
    clippy::expect_used,
    reason = "test scaffolding: a failed temp dir, runtime, or store reply is an unrecoverable fault"
)]

use std::path::Path;
use std::sync::{Arc, Mutex};

use pos_edge::backup::{self, ArchiveKey};
use pos_edge::backup_client::BackupClient;
use pos_ports::cloud_sync::{ActivationGrant, CloudSync, SignedArtifact};
use pos_ports::{PortError, PortName, UpdateReport};
use pos_proto::ids::{BillId, StoreId};
use pos_proto::text::ReleaseTag;
use pos_proto::time::Timestamp;
use pos_proto::ulid::Ulid;
use store_sqlite::SqliteStore;
use tempfile::TempDir;

/// The key this stub cloud issues. Fixed, so the test can open what the loop sealed.
const ISSUED_KEY: &str = "0f1e2d3c4b5a69788796a5b4c3d2e1f00f1e2d3c4b5a69788796a5b4c3d2e1f0";

fn this_shop() -> StoreId {
    StoreId::new(Ulid::from_u128(0xB00))
}

/// One archive as the stub cloud received it: the recovery point the store claimed, and the bytes.
#[derive(Debug, Clone)]
struct Received {
    taken_at: i64,
    bytes: Vec<u8>,
}

/// A cloud that issues one key and keeps every archive it is handed.
#[derive(Debug, Clone, Default)]
struct StubCloud {
    received: Arc<Mutex<Vec<Received>>>,
}

impl StubCloud {
    fn archives(&self) -> Vec<Received> {
        self.received
            .lock()
            .expect("the archive list is not poisoned")
            .clone()
    }
}

impl CloudSync for StubCloud {
    async fn activate(&self, _activation_code: &str) -> Result<ActivationGrant, PortError> {
        Err(PortError::unavailable(
            PortName::CloudSync,
            "this stub only archives",
        ))
    }

    async fn fetch_update(&self, _release: &ReleaseTag) -> Result<SignedArtifact, PortError> {
        Err(PortError::unavailable(
            PortName::CloudSync,
            "this stub only archives",
        ))
    }

    async fn report(&self, _report: &UpdateReport) -> Result<(), PortError> {
        Err(PortError::unavailable(
            PortName::CloudSync,
            "this stub only archives",
        ))
    }

    async fn archive_key(&self, _store: StoreId) -> Result<String, PortError> {
        Ok(ISSUED_KEY.to_owned())
    }

    async fn upload_archive(
        &self,
        _store: StoreId,
        taken_at: Timestamp,
        archive: &[u8],
    ) -> Result<(), PortError> {
        self.received
            .lock()
            .expect("the archive list is not poisoned")
            .push(Received {
                taken_at: taken_at.as_milliseconds_since_epoch(),
                bytes: archive.to_vec(),
            });
        Ok(())
    }
}

/// A till that has traded: fifty settled bills with receipt numbers.
///
/// Returns the open store as well as the numbers, so a caller can keep the database *live* across
/// the archive. That is the case worth testing — `VACUUM INTO` runs beside the writer, and a
/// snapshot taken only of a closed, checkpointed file would prove nothing about a shop mid-service.
async fn a_till_that_has_traded(path: &Path) -> (SqliteStore, Vec<u64>) {
    let store = SqliteStore::open(path).expect("open the store");
    let mut numbers = Vec::new();
    for bill in 1..=50_u128 {
        numbers.push(
            store
                .allocate_receipt_number(this_shop(), BillId::new(Ulid::from_u128(bill)))
                .await
                .expect("allocate a receipt number"),
        );
    }
    (store, numbers)
}

/// One round of the loop, against a live database, and back again.
///
/// The two assertions are the whole point of sealing at the till: what left the box is not the
/// database — the buyer details a store holds (ADR-0107) are not in those bytes in the clear — and
/// what left the box *is* the database, to anyone holding the key.
#[tokio::test(flavor = "multi_thread")]
async fn one_round_ships_a_sealed_copy_of_the_live_store() {
    let dir = TempDir::new().expect("temp dir");
    let live = dir.path().join("store.sqlite");
    // Held open for the whole round: the archive is taken from a store that is still trading.
    let (open_store, issued) = a_till_that_has_traded(&live).await;

    let cloud = StubCloud::default();
    let client = BackupClient::new(
        cloud.clone(),
        this_shop(),
        live.clone(),
        core::time::Duration::from_secs(86_400),
    );
    let size = client.archive_once().await.expect("one round of archiving");

    let received = cloud.archives();
    assert_eq!(received.len(), 1, "one round ships exactly one archive");
    let Received { taken_at, bytes } = &received[0];
    assert_eq!(bytes.len(), size, "the reported size is what was shipped");
    assert!(
        *taken_at > 0,
        "the recovery point is stamped, not left at the epoch"
    );
    assert!(
        bytes.starts_with(b"P4PSTORE"),
        "what the cloud received is an archive, not a database"
    );
    assert!(
        !bytes.windows(15).any(|window| window == b"SQLite format 3"),
        "the database header is not in the bytes that left the box"
    );

    // The plaintext working copy is not left lying beside the live database.
    assert!(
        !dir.path().join("store.sqlite.snapshot").exists(),
        "the round removes its own working copy, sealed or not"
    );

    // And with the key the cloud issued, the archive is the store again.
    let bench = TempDir::new().expect("temp dir");
    let restored = bench.path().join("store.sqlite");
    let key = ArchiveKey::parse(ISSUED_KEY).expect("a 64-character key");
    backup::open_file(bytes, &key, this_shop(), &restored).expect("open what the cloud received");
    let (_restored_store, reissued) = a_till_that_has_traded(&restored).await;
    assert_eq!(
        reissued, issued,
        "the restored store re-issues every bill its own receipt number, so the gapless counter \
         came across the wire"
    );
    drop(open_store);
}

/// A cloud that will not issue a key ends the round, and ships nothing.
///
/// The property is that the failure is *before* the snapshot: a box whose cloud is down must not
/// spend a `VACUUM INTO` of its whole database every interval to discover that.
#[tokio::test(flavor = "multi_thread")]
async fn a_cloud_that_issues_no_key_costs_no_snapshot() {
    #[derive(Debug, Clone)]
    struct Refusing;

    impl CloudSync for Refusing {
        async fn activate(&self, _activation_code: &str) -> Result<ActivationGrant, PortError> {
            Err(PortError::unavailable(PortName::CloudSync, "down"))
        }
        async fn fetch_update(&self, _release: &ReleaseTag) -> Result<SignedArtifact, PortError> {
            Err(PortError::unavailable(PortName::CloudSync, "down"))
        }
        async fn report(&self, _report: &UpdateReport) -> Result<(), PortError> {
            Err(PortError::unavailable(PortName::CloudSync, "down"))
        }
        async fn archive_key(&self, _store: StoreId) -> Result<String, PortError> {
            Err(PortError::unavailable(PortName::CloudSync, "down"))
        }
        async fn upload_archive(
            &self,
            _store: StoreId,
            _taken_at: Timestamp,
            _archive: &[u8],
        ) -> Result<(), PortError> {
            Err(PortError::unavailable(PortName::CloudSync, "down"))
        }
    }

    let dir = TempDir::new().expect("temp dir");
    let live = dir.path().join("store.sqlite");
    let (_store, _numbers) = a_till_that_has_traded(&live).await;

    let client = BackupClient::new(
        Refusing,
        this_shop(),
        live,
        core::time::Duration::from_secs(86_400),
    );
    assert!(
        client.archive_once().await.is_err(),
        "a cloud that will not issue a key ends the round"
    );
    assert!(
        !dir.path().join("store.sqlite.snapshot").exists(),
        "and no snapshot was taken to find that out"
    );
}
