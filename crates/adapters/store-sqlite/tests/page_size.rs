// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! A store created before 8 KiB pages is rebuilt at 8 KiB, once, and keeps everything (finding F5).
//!
//! The case that matters is an old store: a file whose first page was written at 4 KiB, with a
//! schema and events in it, that `SqliteStore::open` has been opening for months and silently leaving
//! at 4 KiB. After the rebuild it is at 8 KiB, smaller, holds the same events, and its chain still
//! verifies.

#![allow(
    clippy::expect_used,
    reason = "test scaffolding: a failed temp dir, runtime, or writer reply is an unrecoverable fault"
)]

use core::future::Future;
use core::num::NonZeroU32;
use std::path::Path;

use pos_ports::event_store::{EventQuery, EventStore};
use pos_ports::{Transactional, TxContext};
use pos_proto::chain::ChainStatus;
use pos_proto::envelope::{EventEnvelope, EventTypeRef, RawPayload};
use pos_proto::events::EventType;
use pos_proto::ids::{BrandId, DeviceId, EventId, StoreId, TenantId};
use pos_proto::time::{BusinessDate, Timestamp};
use pos_proto::ulid::Ulid;
use store_sqlite::{PAGE_SIZE, PageRebuild, SqliteStore, needs_page_rebuild, rebuild_page_size};

fn block_on<F: Future>(future: F) -> F::Output {
    tokio::runtime::Builder::new_current_thread()
        .build()
        .expect("build a current-thread tokio runtime")
        .block_on(future)
}

fn store_id() -> StoreId {
    StoreId::new(Ulid::from_u128(0xB00))
}

fn envelope(n: u128) -> EventEnvelope<RawPayload> {
    EventEnvelope {
        event_id: EventId::new(Ulid::from_u128(0x1000 + n)),
        event_type: EventTypeRef::from_known(EventType::BillingBillSettled),
        event_time: Timestamp::from_milliseconds_since_epoch(
            1_767_225_600_000 + i64::try_from(n).expect("small") * 1_000,
        )
        .expect("instant"),
        business_date: BusinessDate::from_ymd(2026, 1, 1).expect("date"),
        schema_version: 1,
        tenant_id: TenantId::new(Ulid::from_u128(1)),
        brand_id: BrandId::new(Ulid::from_u128(2)),
        store_id: store_id(),
        device_id: DeviceId::new(Ulid::from_u128(3)),
        employee_id: None,
        shift_id: None,
        chain: None,
        // About the size of a real envelope, which is what made 4 KiB pages overflow.
        data: RawPayload::encode(&serde_json::json!({ "n": n, "pad": "x".repeat(700) }))
            .expect("payload"),
    }
}

fn page_size(path: &Path) -> i64 {
    rusqlite::Connection::open(path)
        .expect("open")
        .query_row("PRAGMA page_size", [], |row| row.get(0))
        .expect("read")
}

#[test]
fn an_old_store_is_rebuilt_at_8_kib_and_keeps_every_event() {
    let dir = tempfile::tempdir().expect("temp dir");
    let path = dir.path().join("store.db");
    // A store from before the change: its first page was written at 4 KiB.
    rusqlite::Connection::open(&path)
        .expect("create")
        .execute_batch("PRAGMA page_size = 4096; CREATE TABLE predates_the_schema (x);")
        .expect("seed a 4 KiB file");
    {
        let store = SqliteStore::open(&path).expect("open the old store");
        block_on(async {
            for batch in 0..4_u128 {
                let events: Vec<_> = (batch * 100..batch * 100 + 100).map(envelope).collect();
                let mut tx = store.begin().await.expect("begin");
                store.append(&mut tx, &events).await.expect("append");
                tx.commit().await.expect("commit");
            }
        });
    }
    assert_eq!(page_size(&path), 4096, "opening never changed it");
    assert!(needs_page_rebuild(&path).expect("read"));
    let before = std::fs::metadata(&path).expect("size").len();

    let outcome = rebuild_page_size(&path).expect("rebuild");
    assert!(
        matches!(outcome, PageRebuild::Rebuilt { from: 4096, .. }),
        "{outcome:?}"
    );
    assert_eq!(page_size(&path), i64::from(PAGE_SIZE));
    assert!(!needs_page_rebuild(&path).expect("read"));
    let after = std::fs::metadata(&path).expect("size").len();
    assert!(
        after < before,
        "the rebuilt file is smaller: {after} < {before}"
    );
    assert_eq!(
        rebuild_page_size(&path).expect("rebuild again"),
        PageRebuild::NotNeeded,
        "a second rebuild does nothing"
    );

    let store = SqliteStore::open(&path).expect("reopen");
    let kept = block_on(store.read(&EventQuery::first(
        store_id(),
        NonZeroU32::new(1_000).expect("positive"),
    )))
    .expect("read");
    assert_eq!(kept.len(), 400, "every event is still there");
    assert!(matches!(
        block_on(store.verify_chain(store_id())).expect("verify"),
        ChainStatus::Intact { checked: 400, .. }
    ));
    let journal: String = rusqlite::Connection::open(&path)
        .expect("open")
        .query_row("PRAGMA journal_mode", [], |row| row.get(0))
        .expect("read");
    assert_eq!(journal, "wal", "back in WAL mode");
}

/// A new store, and no store at all, need nothing.
#[test]
fn a_new_store_needs_no_rebuild() {
    let dir = tempfile::tempdir().expect("temp dir");
    let path = dir.path().join("store.db");
    assert_eq!(
        rebuild_page_size(&path).expect("absent"),
        PageRebuild::NotNeeded
    );
    drop(SqliteStore::open(&path).expect("create"));
    assert_eq!(page_size(&path), i64::from(PAGE_SIZE));
    assert_eq!(
        rebuild_page_size(&path).expect("new"),
        PageRebuild::NotNeeded
    );
}
