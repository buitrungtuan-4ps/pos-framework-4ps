// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! A shop's database leaves the shop sealed, and comes back a shop (ADR-0124).
//!
//! The headline is `a_sealed_archive_restores_to_a_working_store`: take a till that has traded,
//! seal a snapshot of it the way the backup loop will, open it on a different machine, and prove
//! the restored file is a database the edge can open — with the same receipt numbers, the same
//! events, and the same buyer details. Everything else here is about the ways that must *fail*:
//! the wrong key, another shop's archive, an altered byte, a truncated download. All four are one
//! error on purpose — an authenticated cipher cannot tell them apart, and a message that guessed
//! would be a message that misleads.

// The whole file is test scaffolding; a failed temp dir or runtime is an unrecoverable setup fault.
#![allow(
    clippy::expect_used,
    reason = "test scaffolding: a failed temp dir, runtime, or store reply is an unrecoverable fault"
)]

use std::future::Future;
use std::path::Path;

use pos_edge::backup::{self, ArchiveKey};
use pos_ports::subject_store::{SubjectRecord, SubjectStore};
use pos_ports::tx::{Transactional, TxContext};
use pos_proto::ids::{BillId, StoreId, SubjectId};
use pos_proto::time::Timestamp;
use pos_proto::ulid::Ulid;
use store_sqlite::{SqliteStore, snapshot_to};
use tempfile::TempDir;

fn block_on<F: Future>(future: F) -> F::Output {
    tokio::runtime::Builder::new_current_thread()
        .build()
        .expect("build a current-thread tokio runtime")
        .block_on(future)
}

fn this_shop() -> StoreId {
    StoreId::new(Ulid::from_u128(0xB00))
}

fn another_shop() -> StoreId {
    StoreId::new(Ulid::from_u128(0xB01))
}

fn key() -> ArchiveKey {
    ArchiveKey::parse("00112233445566778899aabbccddeeff00112233445566778899aabbccddeeff")
        .expect("a 64-character key")
}

fn other_key() -> ArchiveKey {
    ArchiveKey::parse("ffeeddccbbaa99887766554433221100ffeeddccbbaa99887766554433221100")
        .expect("a 64-character key")
}

/// A till that has traded: fifty settled bills with receipt numbers, and one corporate buyer whose
/// details live in `subjects` and nowhere else (ADR-0107).
fn a_till_that_has_traded(path: &Path) -> Vec<u64> {
    block_on(async {
        let store = SqliteStore::open(path).expect("open the store");
        let mut numbers = Vec::new();
        for bill in 1..=50_u128 {
            numbers.push(
                store
                    .allocate_receipt_number(this_shop(), BillId::new(Ulid::from_u128(bill)))
                    .await
                    .expect("allocate"),
            );
        }
        let mut tx = store.begin().await.expect("begin");
        store
            .record(
                &mut tx,
                this_shop(),
                SubjectId::new(Ulid::from_u128(0xB0B)),
                &SubjectRecord {
                    collected_at: Timestamp::from_milliseconds_since_epoch(1_700_000_000_000)
                        .expect("a valid instant"),
                    fields: [("name".to_owned(), "Công ty TNHH Bốn Phương".to_owned())]
                        .into_iter()
                        .collect(),
                    masked_at: None,
                },
            )
            .await
            .expect("record the buyer");
        tx.commit().await.expect("commit");
        drop(store);
        numbers
    })
}

fn sealed_archive_of(path: &Path, work: &Path) -> Vec<u8> {
    let snapshot = work.join("snapshot.sqlite");
    snapshot_to(path, &snapshot).expect("snapshot");
    backup::seal_file(&snapshot, &key(), this_shop()).expect("seal")
}

#[test]
fn a_sealed_archive_restores_to_a_working_store() {
    let dir = TempDir::new().expect("temp dir");
    let live = dir.path().join("store.sqlite");
    let issued = a_till_that_has_traded(&live);

    let archive = sealed_archive_of(&live, dir.path());
    assert!(
        u64::try_from(archive.len()).expect("fits")
            < std::fs::metadata(&live).expect("metadata").len(),
        "the archive is smaller than the database it holds — it is deflated before it is sealed"
    );

    // The replacement machine: a different directory, nothing but the archive and the key.
    let bench = TempDir::new().expect("temp dir");
    let restored = bench.path().join("store.sqlite");
    backup::open_file(&archive, &key(), this_shop(), &restored).expect("open the archive");

    let (numbers, buyer) = block_on(async {
        let store = SqliteStore::open(&restored).expect("the restored file opens as a store");
        let mut numbers = Vec::new();
        for bill in 1..=50_u128 {
            numbers.push(
                store
                    .allocate_receipt_number(this_shop(), BillId::new(Ulid::from_u128(bill)))
                    .await
                    .expect("allocate"),
            );
        }
        let buyer = store
            .fetch(this_shop(), SubjectId::new(Ulid::from_u128(0xB0B)))
            .await
            .expect("fetch the buyer");
        (numbers, buyer)
    });

    assert_eq!(
        numbers, issued,
        "the restored store re-issues every bill its own receipt number — the gapless counter came \
         across, so the replacement does not start again at one"
    );
    assert_eq!(
        buyer.expect("the buyer is in the restored store").fields["name"],
        "Công ty TNHH Bốn Phương",
        "the B2B buyer's details survived the round trip, non-ASCII and all"
    );
}

#[test]
fn the_wrong_key_does_not_open_an_archive() {
    let dir = TempDir::new().expect("temp dir");
    let live = dir.path().join("store.sqlite");
    let _ = a_till_that_has_traded(&live);
    let archive = sealed_archive_of(&live, dir.path());

    let refused = backup::open(&archive, &other_key(), this_shop());

    assert!(
        matches!(refused, Err(backup::ArchiveError::Unsealable)),
        "an archive is bytes without its key, not a database: {refused:?}"
    );
}

#[test]
fn one_shops_archive_will_not_open_as_another_shops() {
    let dir = TempDir::new().expect("temp dir");
    let live = dir.path().join("store.sqlite");
    let _ = a_till_that_has_traded(&live);
    let archive = sealed_archive_of(&live, dir.path());

    // The failure this prevents is not theft, it is a mix-up: a fleet of archives in one bucket,
    // named by the operator who is restoring at midnight, and one shop's trading restored onto
    // another shop's till. The store id is sealed in as associated data, so the mistake refuses.
    let refused = backup::open(&archive, &key(), another_shop());

    assert!(
        matches!(refused, Err(backup::ArchiveError::Unsealable)),
        "an archive is bound to the shop it came from: {refused:?}"
    );
}

#[test]
fn an_altered_archive_does_not_open() {
    let dir = TempDir::new().expect("temp dir");
    let live = dir.path().join("store.sqlite");
    let _ = a_till_that_has_traded(&live);
    let mut archive = sealed_archive_of(&live, dir.path());

    let last = archive.len() - 1;
    archive[last] ^= 0x01;

    assert!(
        matches!(
            backup::open(&archive, &key(), this_shop()),
            Err(backup::ArchiveError::Unsealable)
        ),
        "one flipped bit is caught: the archive is authenticated, not merely encrypted"
    );
}

#[test]
fn a_truncated_download_does_not_open_and_leaves_no_file_behind() {
    let dir = TempDir::new().expect("temp dir");
    let live = dir.path().join("store.sqlite");
    let _ = a_till_that_has_traded(&live);
    let archive = sealed_archive_of(&live, dir.path());

    let half = archive.len() / 2;
    let cut = archive.get(..half).expect("half of it").to_vec();

    let bench = TempDir::new().expect("temp dir");
    let restored = bench.path().join("store.sqlite");
    let refused = backup::open_file(&cut, &key(), this_shop(), &restored);

    assert!(refused.is_err(), "half an archive is not a store");
    assert!(
        !restored.exists(),
        "and a refused restore leaves nothing on disk that looks like one"
    );
}

#[test]
fn bytes_that_are_not_an_archive_say_so_rather_than_failing_to_decrypt() {
    // A different message on purpose: "you gave me the wrong file" is actionable, where "this did
    // not open" sends an operator hunting for a key that was never the problem.
    let refused = backup::open(b"SQLite format 3\0and then some", &key(), this_shop());
    assert!(
        matches!(refused, Err(backup::ArchiveError::NotAnArchive)),
        "a plain database is not an archive: {refused:?}"
    );
}

#[test]
fn a_key_round_trips_through_the_text_an_operator_keeps() {
    let minted = ArchiveKey::generate().expect("entropy");
    let text = minted.to_text();
    assert_eq!(text.len(), ArchiveKey::TEXT_LEN);

    let dir = TempDir::new().expect("temp dir");
    let live = dir.path().join("store.sqlite");
    let _ = a_till_that_has_traded(&live);
    let snapshot = dir.path().join("snapshot.sqlite");
    snapshot_to(&live, &snapshot).expect("snapshot");
    let archive = backup::seal_file(&snapshot, &minted, this_shop()).expect("seal");

    // Typed back in from a printout, with the shouting and the stray spaces an operator's copy has.
    let retyped = ArchiveKey::parse(&format!("  {}  ", text.to_uppercase())).expect("parse");

    assert!(
        backup::open(&archive, &retyped, this_shop()).is_ok(),
        "a key written down and typed back in still opens the archive"
    );
    assert!(
        ArchiveKey::parse("not a key").is_err(),
        "and something that is not a key is refused rather than truncated into one"
    );
}
