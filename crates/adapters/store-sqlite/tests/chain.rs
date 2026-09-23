// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The hash chain over the event log ([ADR-0131](../../../../docs/adr/0131-a-chained-event-log.md)).
//!
//! Every case here tampers with the database **the way somebody with access to the shop's PC
//! would** — plain SQL against the file — and then asks the store what it finds. Two of them
//! deliberately assert that the chain finds **nothing**, because those are the cases a chain
//! cannot catch alone and the record is explicit that they need the cloud anchor. A test suite
//! that quietly omitted them would leave the reader believing the chain is stronger than it is.

// The whole file is test scaffolding; a failed temp dir or runtime is an unrecoverable setup fault.
#![allow(
    clippy::expect_used,
    reason = "test scaffolding: a failed temp dir, runtime, or writer reply is an unrecoverable fault"
)]

use core::future::Future;
use std::path::Path;

use pos_ports::event_store::EventStore;
use pos_ports::{Transactional, TxContext};
use pos_proto::chain::{ChainBreak, ChainStatus};
use pos_proto::envelope::{EventEnvelope, EventTypeRef, RawPayload};
use pos_proto::events::EventType;
use pos_proto::ids::{BrandId, DeviceId, EventId, StoreId, TenantId};
use pos_proto::time::{BusinessDate, Timestamp};
use pos_proto::ulid::Ulid;
use store_sqlite::SqliteStore;
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

fn envelope(seed: u128, amount_minor: i64) -> EventEnvelope<RawPayload> {
    EventEnvelope {
        event_id: EventId::new(Ulid::from_u128(seed)),
        event_type: EventTypeRef::from_known(EventType::BillingBillSettled),
        event_time: Timestamp::from_milliseconds_since_epoch(1_767_225_600_000).expect("instant"),
        business_date: BusinessDate::from_ymd(2026, 8, 13).expect("date"),
        schema_version: 1,
        tenant_id: TenantId::new(Ulid::from_u128(1)),
        brand_id: BrandId::new(Ulid::from_u128(2)),
        store_id: store_id(),
        device_id: DeviceId::new(Ulid::from_u128(3)),
        employee_id: None,
        shift_id: None,
        chain: None,
        data: RawPayload::encode(&serde_json::json!({ "amount_minor": amount_minor }))
            .expect("payload"),
    }
}

/// Four settled bills, written the way an edge writes them.
async fn seed(store: &SqliteStore) {
    for (seed, amount) in [(1, 500_000), (2, 1_200_000), (3, 800_000), (4, 300_000)] {
        let mut tx = store.begin().await.expect("begin");
        store
            .append(&mut tx, &[envelope(seed, amount)])
            .await
            .expect("append");
        tx.commit().await.expect("commit");
    }
}

/// Opens the file directly, exactly as somebody standing at the shop's PC would.
fn tamper(path: &Path, sql: &str) {
    let connection = rusqlite::Connection::open(path).expect("open the file");
    connection.execute_batch(sql).expect("tamper");
}

fn verify(store: &SqliteStore) -> ChainStatus {
    block_on(store.verify_chain(store_id())).expect("verify")
}

struct Fixture {
    _dir: TempDir,
    path: std::path::PathBuf,
}

impl Fixture {
    fn new() -> (Self, SqliteStore) {
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join("store.db");
        let store = SqliteStore::open(&path).expect("open");
        block_on(seed(&store));
        (Self { _dir: dir, path }, store)
    }

    /// Reopens after tampering, because the walk reads what is on disk.
    fn reopen(&self) -> SqliteStore {
        SqliteStore::open(&self.path).expect("reopen")
    }

    /// A store with `count` chained records, for the cases that must cross a chunk boundary.
    ///
    /// Batched rather than one transaction per event: the point is a log longer than
    /// `CHAIN_CHUNK`, and writing a couple of thousand single-event transactions to prove it
    /// would make the case slow enough that someone would eventually delete it.
    fn long(count: u128) -> (Self, SqliteStore) {
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join("store.db");
        let store = SqliteStore::open(&path).expect("open");
        block_on(async {
            let mut next = 1_u128;
            while next <= count {
                let upto = (next + 250).min(count + 1);
                let batch: Vec<_> = (next..upto).map(|seed| envelope(seed, 500_000)).collect();
                let mut tx = store.begin().await.expect("begin");
                store.append(&mut tx, &batch).await.expect("append");
                tx.commit().await.expect("commit");
                next = upto;
            }
        });
        (Self { _dir: dir, path }, store)
    }
}

/// The walk is split into bounded chunks so it cannot hold the single writer thread for the length
/// of the log (ADR-0015), and the three cases below are the ones that split can get wrong. All of
/// them need a log longer than one chunk, which every other case in this file is deliberately too
/// small to produce.
///
/// `CHAIN_CHUNK` is 1,000, so 1,500 records means two chunks with the boundary at 1,000 — and the
/// interesting record is the first one of the second chunk, `seq = 1001`, which is the one whose
/// `prev_hash` has to be carried across.
const LONGER_THAN_A_CHUNK: u128 = 1_500;

#[test]
fn a_log_longer_than_one_chunk_still_verifies_whole() {
    // A false break at the boundary would show up here: the second chunk starts mid-chain, and a
    // walk that restarted from genesis instead of from the previous chunk's hash would report a
    // `LinkMismatch` on a log nobody has touched.
    let (_fixture, store) = Fixture::long(LONGER_THAN_A_CHUNK);
    match verify(&store) {
        ChainStatus::Intact { checked, .. } => assert_eq!(
            checked,
            u64::try_from(LONGER_THAN_A_CHUNK).expect("count fits"),
            "every record across both chunks is counted exactly once"
        ),
        other @ ChainStatus::Broken { .. } => {
            panic!("a log longer than a chunk must still verify: {other:?}")
        }
    }
}

#[test]
fn an_edit_on_the_first_record_of_the_second_chunk_is_caught() {
    // The case the carry exists for. If `ChainCursor` dropped the hash at the boundary, the first
    // record of the second chunk would be checked against genesis — or against nothing — and an
    // edit here would pass. It is the one record in the log a chunking mistake hides.
    let (fixture, _store) = Fixture::long(LONGER_THAN_A_CHUNK);
    tamper(
        &fixture.path,
        "UPDATE events SET envelope = replace(envelope, '500000', '9500000') WHERE seq = 1001",
    );
    match verify(&fixture.reopen()) {
        ChainStatus::Broken { at_seq, reason } => {
            assert_eq!(
                at_seq, 1001,
                "the break is named at the record that was edited"
            );
            assert_eq!(reason, ChainBreak::ContentEdited);
        }
        other @ ChainStatus::Intact { .. } => {
            panic!("an edit on the first record of the second chunk must be caught: {other:?}")
        }
    }
}

#[test]
fn a_record_deleted_at_the_start_of_the_second_chunk_is_named_as_a_gap() {
    // The other half of the carry: the `seq` anchor.
    //
    // It has to be `seq = 1001` and not 1000. Deleting 1000 is caught *inside* the first chunk —
    // the walk reaches 1001 while still counting, sees the position jump, and reports the gap
    // before any boundary is crossed. That version of this test passed with the anchor
    // deliberately broken, which is to say it was not testing the boundary at all.
    //
    // Deleting 1001 puts the gap exactly at the seam: the first chunk ends cleanly on 1000, and
    // the second opens on 1002. A chunk that restarted its gap check would take 1002 as "the
    // first record I have seen", find no gap, and then report the wrong fault at the wrong
    // position — `LinkMismatch` at 1002 rather than `MissingRecord` at 1001. Both fields are
    // asserted for that reason.
    let (fixture, _store) = Fixture::long(LONGER_THAN_A_CHUNK);
    tamper(&fixture.path, "DELETE FROM events WHERE seq = 1001");
    match verify(&fixture.reopen()) {
        ChainStatus::Broken { at_seq, reason } => {
            assert_eq!(
                at_seq, 1001,
                "the gap is named at the position that went missing"
            );
            assert_eq!(reason, ChainBreak::MissingRecord);
        }
        other @ ChainStatus::Intact { .. } => {
            panic!("a record deleted at the chunk boundary must be caught: {other:?}")
        }
    }
}

#[test]
fn an_untouched_log_verifies_and_reports_its_head() {
    let (_fixture, store) = Fixture::new();
    match verify(&store) {
        ChainStatus::Intact {
            checked,
            unchained,
            head,
        } => {
            assert_eq!(checked, 4, "all four records are chained");
            assert_eq!(unchained, 0, "nothing predates the chain in a fresh store");
            let head = head.expect("a chained store has a head");
            assert_eq!(head.as_str().len(), 64);
            assert!(!head.is_genesis(), "the head is a real digest, not genesis");
        }
        other @ ChainStatus::Broken { .. } => {
            panic!("a freshly written log must verify: {other:?}")
        }
    }
}

/// The headline case: 1,800,000 VND edited away, which the bare three-column table reported as
/// `integrity_check: ok`.
#[test]
fn editing_an_amount_is_caught_and_named() {
    let (fixture, _store) = Fixture::new();
    tamper(
        &fixture.path,
        "UPDATE events
         SET envelope = replace(envelope, '1200000', '200000')
         WHERE seq = 2",
    );
    match verify(&fixture.reopen()) {
        ChainStatus::Broken { at_seq, reason } => {
            assert_eq!(at_seq, 2);
            assert_eq!(reason, ChainBreak::ContentEdited);
        }
        other @ ChainStatus::Intact { .. } => {
            panic!("an edited amount must break the chain: {other:?}")
        }
    }
}

#[test]
fn deleting_a_record_from_the_middle_is_caught() {
    let (fixture, _store) = Fixture::new();
    tamper(&fixture.path, "DELETE FROM events WHERE seq = 2");
    match verify(&fixture.reopen()) {
        ChainStatus::Broken { at_seq, reason } => {
            assert_eq!(at_seq, 2, "the gap is found where the missing record sat");
            assert_eq!(reason, ChainBreak::MissingRecord);
        }
        other @ ChainStatus::Intact { .. } => {
            panic!("a deleted record must break the chain: {other:?}")
        }
    }
}

#[test]
fn relinking_a_record_to_the_wrong_predecessor_is_caught() {
    let (fixture, _store) = Fixture::new();
    tamper(
        &fixture.path,
        "UPDATE events SET prev_hash = 'ff00ff00ff00ff00ff00ff00ff00ff00ff00ff00ff00ff00ff00ff00ff00ff00'
         WHERE seq = 3",
    );
    match verify(&fixture.reopen()) {
        ChainStatus::Broken { at_seq, reason } => {
            assert_eq!(at_seq, 3);
            assert_eq!(reason, ChainBreak::LinkMismatch);
        }
        other @ ChainStatus::Intact { .. } => {
            panic!("a re-linked record must break the chain: {other:?}")
        }
    }
}

/// **A chain alone does not catch this, and the test says so.**
///
/// Cutting the tail off leaves everything that survives perfectly linked. Nothing is
/// inconsistent; the log simply ends earlier. Only the head published to the cloud disagrees —
/// which is the other half of ADR-0131 and is not in this change.
#[test]
fn truncating_the_tail_is_not_caught_by_the_chain_alone() {
    let (fixture, _store) = Fixture::new();
    let head_before = match verify(&fixture.reopen()) {
        ChainStatus::Intact { head, .. } => head.expect("a head"),
        other @ ChainStatus::Broken { .. } => panic!("seeded log must verify: {other:?}"),
    };

    tamper(&fixture.path, "DELETE FROM events WHERE seq >= 3");

    match verify(&fixture.reopen()) {
        ChainStatus::Intact { checked, head, .. } => {
            assert_eq!(checked, 2, "two records survive, and they link cleanly");
            let head_after = head.expect("a head");
            assert_ne!(
                head_after, head_before,
                "the head moved, which is the only trace — and only an anchor the store cannot \
                 reach can notice it"
            );
        }
        other @ ChainStatus::Broken { .. } => panic!(
            "a truncated chain still verifies against itself; this test exists to record that: \
             {other:?}"
        ),
    }
}

/// **A chain alone does not catch this either.**
///
/// The algorithm is in the source. Edit a record, re-derive every later link, and the result
/// verifies cleanly. This is the case that makes "tamper-evident" the honest word and
/// "tamper-proof" a lie, and the reason the anchor is in the decision rather than a follow-up.
#[test]
fn editing_and_recomputing_the_whole_chain_is_not_caught_by_the_chain_alone() {
    let (fixture, _store) = Fixture::new();
    let head_before = match verify(&fixture.reopen()) {
        ChainStatus::Intact { head, .. } => head.expect("a head"),
        other @ ChainStatus::Broken { .. } => panic!("seeded log must verify: {other:?}"),
    };

    // Rebuild the file the way an attacker with the source would: write the edited history from
    // scratch through the store's own append path, so every link is derived correctly.
    let dir = tempfile::tempdir().expect("temp dir");
    let forged_path = dir.path().join("forged.db");
    let forged = SqliteStore::open(&forged_path).expect("open");
    block_on(async {
        for (seed, amount) in [(1, 500_000), (2, 1), (3, 800_000), (4, 300_000)] {
            let mut tx = forged.begin().await.expect("begin");
            forged
                .append(&mut tx, &[envelope(seed, amount)])
                .await
                .expect("append");
            tx.commit().await.expect("commit");
        }
    });

    match block_on(forged.verify_chain(store_id())).expect("verify") {
        ChainStatus::Intact { checked, head, .. } => {
            assert_eq!(checked, 4);
            assert_ne!(
                head.expect("a head"),
                head_before,
                "the forged head differs from the real one — the only thing that gives it away, \
                 and only to somebody holding the real one"
            );
        }
        other @ ChainStatus::Broken { .. } => panic!(
            "a recomputed chain verifies against itself; this test exists to record that: {other:?}"
        ),
    }
}

/// A store that upgrades mid-life keeps its old rows and starts chaining at the next event.
///
/// Modelled the way an upgrade actually happens: rows already in the table with no chain columns
/// (migration 0001 wrote three columns), then new events appended through the store's own path.
/// Those old rows are **unchained**, which is a different thing from broken, and nothing backfills
/// them — a chain over history nobody can vouch for is the false confidence the record rejects.
#[test]
fn records_written_before_the_migration_are_unchained_not_broken() {
    let dir = tempfile::tempdir().expect("temp dir");
    let path = dir.path().join("upgraded.db");
    {
        // Open once so the migrations run, then write rows as the pre-chain schema left them.
        let _store = SqliteStore::open(&path).expect("open");
    }
    tamper(
        &path,
        &format!(
            "INSERT INTO events (store_id, event_id, envelope) VALUES
               ('{store}', '01OLD0000000000000000001', '{{}}'),
               ('{store}', '01OLD0000000000000000002', '{{}}')",
            store = store_id()
        ),
    );

    let store = SqliteStore::open(&path).expect("reopen");
    block_on(seed(&store));

    match verify(&store) {
        ChainStatus::Intact {
            checked, unchained, ..
        } => {
            assert_eq!(
                unchained, 2,
                "the pre-migration rows are counted, not blamed"
            );
            assert_eq!(
                checked, 4,
                "and the chain starts at the first event written after the upgrade"
            );
        }
        other @ ChainStatus::Broken { .. } => panic!("unchained history is not a break: {other:?}"),
    }
}

/// Stripping the chain columns out of the middle of a live chain **is** tampering, and is caught.
///
/// Worth its own case because it is the obvious way to try to use the "unchained is not broken"
/// rule as a hiding place: edit a record, then blank its columns so the walk skips it.
///
/// It surfaces as a **gap at the hole's own position**, not as a link mismatch at the record
/// after it: the walk notices the sequence skipping before it looks at any hash. That is the more
/// useful of the two reports, because it names where the missing record sat rather than the
/// innocent row that followed it.
#[test]
fn blanking_the_chain_columns_mid_log_is_caught() {
    let (fixture, _store) = Fixture::new();
    tamper(
        &fixture.path,
        "UPDATE events SET seq = NULL, prev_hash = NULL, hash = NULL WHERE seq = 2",
    );
    match verify(&fixture.reopen()) {
        ChainStatus::Broken { at_seq, reason } => {
            assert_eq!(at_seq, 2, "the position the blanked record sat at");
            assert_eq!(reason, ChainBreak::MissingRecord);
        }
        other @ ChainStatus::Intact { .. } => {
            panic!("a hole punched in a live chain must break it: {other:?}")
        }
    }
}

#[test]
fn a_duplicate_append_does_not_advance_the_sequence() {
    let (_fixture, store) = Fixture::new();
    block_on(async {
        // The same event_id again — the retry the outbox protocol requires.
        let mut tx = store.begin().await.expect("begin");
        store
            .append(&mut tx, &[envelope(2, 1_200_000)])
            .await
            .expect("append");
        tx.commit().await.expect("commit");
    });
    match verify(&store) {
        ChainStatus::Intact { checked, .. } => assert_eq!(
            checked, 4,
            "a replayed event must not take a sequence number, or it would leave a gap that \
             every later verification reports as a missing record"
        ),
        other @ ChainStatus::Broken { .. } => {
            panic!("a replay must not break the chain: {other:?}")
        }
    }
}
