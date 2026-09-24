// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! Retention: an event goes only once it is synced **and** old, and what stays still verifies
//! ([ADR-0145](../../../../docs/adr/0145-the-edge-keeps-events-until-synced-and-n-days-old.md)).
//!
//! The rules the store itself enforces are the ones tested here: an event the link has not
//! acknowledged stays however old it is, the chain head always stays, deletion is a prefix in
//! commit order, and the chain walk starts from the checkpoint so the kept log is still checked
//! link by link. The caller's part, moving `before` back past anything still open, is the edge's
//! and is tested there.

#![allow(
    clippy::expect_used,
    reason = "test scaffolding: a failed temp dir, runtime, or writer reply is an unrecoverable fault"
)]

use core::future::Future;
use core::num::NonZeroU32;
use std::path::{Path, PathBuf};

use pos_ports::event_store::{EventQuery, EventStore, OutboxPosition};
use pos_ports::{Transactional, TxContext};
use pos_proto::chain::ChainStatus;
use pos_proto::envelope::{EventEnvelope, EventTypeRef, RawPayload};
use pos_proto::events::EventType;
use pos_proto::ids::{BrandId, DeviceId, EventId, StoreId, TenantId};
use pos_proto::time::{BusinessDate, Timestamp};
use pos_proto::ulid::Ulid;
use store_sqlite::SqliteStore;
use tempfile::TempDir;

/// 2026-01-01T00:00:00Z. Event `n` happens `n` minutes after it.
const BASE_MS: i64 = 1_767_225_600_000;

fn block_on<F: Future>(future: F) -> F::Output {
    tokio::runtime::Builder::new_current_thread()
        .build()
        .expect("build a current-thread tokio runtime")
        .block_on(future)
}

fn store_id() -> StoreId {
    StoreId::new(Ulid::from_u128(0xB00))
}

fn minute(n: i64) -> Timestamp {
    Timestamp::from_milliseconds_since_epoch(BASE_MS + n * 60_000).expect("instant")
}

fn far_future() -> Timestamp {
    minute(1_000_000)
}

fn envelope(n: u128) -> EventEnvelope<RawPayload> {
    EventEnvelope {
        event_id: EventId::new(Ulid::from_u128(0x1000 + n)),
        event_type: EventTypeRef::from_known(EventType::BillingBillSettled),
        event_time: minute(i64::try_from(n).expect("small")),
        business_date: BusinessDate::from_ymd(2026, 1, 1).expect("date"),
        schema_version: 1,
        tenant_id: TenantId::new(Ulid::from_u128(1)),
        brand_id: BrandId::new(Ulid::from_u128(2)),
        store_id: store_id(),
        device_id: DeviceId::new(Ulid::from_u128(3)),
        employee_id: None,
        shift_id: None,
        chain: None,
        data: RawPayload::encode(&serde_json::json!({ "n": n })).expect("payload"),
    }
}

struct Fixture {
    _dir: TempDir,
    path: PathBuf,
    store: SqliteStore,
}

impl Fixture {
    fn new() -> Self {
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join("store.db");
        let store = SqliteStore::open(&path).expect("open");
        Self {
            _dir: dir,
            path,
            store,
        }
    }

    /// Events `first..=last`, in batches, the way an edge commits them.
    fn append(&self, first: u128, last: u128) {
        block_on(async {
            let mut next = first;
            while next <= last {
                let upto = (next + 250).min(last + 1);
                let batch: Vec<_> = (next..upto).map(envelope).collect();
                let mut tx = self.store.begin().await.expect("begin");
                self.store.append(&mut tx, &batch).await.expect("append");
                tx.commit().await.expect("commit");
                next = upto;
            }
        });
    }

    /// The link acknowledges the oldest `count` unacknowledged events.
    fn acknowledge(&self, count: u32) {
        block_on(async {
            let batch = self
                .store
                .outbox_batch(
                    store_id(),
                    OutboxPosition::START,
                    NonZeroU32::new(count).expect("positive"),
                )
                .await
                .expect("read the outbox");
            let through = batch.last().expect("something to acknowledge").position;
            self.store
                .acknowledge_outbox(store_id(), through)
                .await
                .expect("acknowledge");
        });
    }

    fn prune(&self, before: Timestamp) -> u64 {
        block_on(self.store.prune_events(store_id(), before)).expect("prune")
    }

    fn kept(&self) -> usize {
        let query = EventQuery::first(store_id(), NonZeroU32::new(10_000).expect("positive"));
        block_on(self.store.read(&query)).expect("read").len()
    }

    fn verify(&self) -> ChainStatus {
        block_on(self.store.verify_chain(store_id())).expect("verify")
    }

    fn sql(path: &Path, statement: &str) {
        let connection = rusqlite::Connection::open(path).expect("open the file");
        connection.execute_batch(statement).expect("run");
    }
}

fn intact_with(status: &ChainStatus, expected: u64) {
    match status {
        ChainStatus::Intact { checked, .. } => {
            assert_eq!(*checked, expected, "records verified from the checkpoint");
        }
        ChainStatus::Broken { at_seq, reason } => {
            panic!("the kept log must verify, broke at {at_seq}: {reason:?}")
        }
    }
}

/// However old, an event the link has not acknowledged stays. This is the rule that lets a store
/// be offline for months and lose nothing.
#[test]
fn an_unsynced_event_is_never_deleted() {
    let fixture = Fixture::new();
    fixture.append(1, 10);
    assert_eq!(fixture.prune(far_future()), 0);
    assert_eq!(fixture.kept(), 10);
}

/// Only the synced prefix goes, and what stays verifies from the checkpoint.
#[test]
fn the_synced_prefix_goes_and_the_rest_still_verifies() {
    let fixture = Fixture::new();
    fixture.append(1, 10);
    fixture.acknowledge(6);

    assert_eq!(fixture.prune(far_future()), 6);
    assert_eq!(fixture.kept(), 4);
    intact_with(&fixture.verify(), 4);
    assert_eq!(
        fixture.prune(far_future()),
        0,
        "a second sweep finds nothing"
    );
}

/// A synced event that is not yet old stays: `before` is exclusive.
#[test]
fn a_young_event_stays_even_when_synced() {
    let fixture = Fixture::new();
    fixture.append(1, 10);
    fixture.acknowledge(10);

    // Events 1 and 2 are before minute 3; event 3 is at it.
    assert_eq!(fixture.prune(minute(3)), 2);
    assert_eq!(fixture.kept(), 8);
    intact_with(&fixture.verify(), 8);
}

/// The chain head always stays, so the next record continues the chain instead of restarting it
/// at `seq` 1, which would reuse positions the cloud already holds.
#[test]
fn the_head_stays_and_the_chain_continues() {
    let fixture = Fixture::new();
    fixture.append(1, 10);
    fixture.acknowledge(10);

    assert_eq!(fixture.prune(far_future()), 9);
    assert_eq!(fixture.kept(), 1);
    intact_with(&fixture.verify(), 1);

    fixture.append(11, 12);
    intact_with(&fixture.verify(), 3);
    let anchor = block_on(fixture.store.chain_head(store_id()))
        .expect("head")
        .expect("a chained store");
    assert_eq!(anchor.seq, 12, "the chain went on from where it was");
}

/// Records from before the chain existed go first, and only once they are synced too.
#[test]
fn records_from_before_the_chain_go_first() {
    let fixture = Fixture::new();
    // Three rows written by a build that had no chain: no `seq`, no hash.
    Fixture::sql(
        &fixture.path,
        &(0..3)
            .map(|n| {
                let envelope =
                    serde_json::to_string(&envelope(900 + n)).expect("serialise an envelope");
                format!(
                    "INSERT INTO events (store_id, event_id, envelope) VALUES ('{}', '{}', '{}');",
                    store_id(),
                    Ulid::from_u128(0x0100 + n),
                    envelope.replace('\'', "''"),
                )
            })
            .collect::<String>(),
    );
    let fixture = Fixture {
        store: SqliteStore::open(&fixture.path).expect("reopen"),
        ..fixture
    };
    fixture.append(1, 3);
    fixture.acknowledge(3);
    assert_eq!(fixture.kept(), 6);

    // Unchained events 900..902 are older than `before`; so are chained 1 and 2; 3 is the head.
    assert_eq!(fixture.prune(minute(1_000)), 5);
    assert_eq!(fixture.kept(), 1);
    intact_with(&fixture.verify(), 1);
}

/// A sweep bigger than one chunk finishes, chunk by chunk.
#[test]
fn a_long_sweep_runs_in_chunks() {
    let fixture = Fixture::new();
    fixture.append(1, 2_100);
    fixture.acknowledge(2_100);

    assert_eq!(fixture.prune(far_future()), 2_099);
    assert_eq!(fixture.kept(), 1);
    intact_with(&fixture.verify(), 1);
}
