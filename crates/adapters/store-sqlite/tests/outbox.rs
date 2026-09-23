// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! A deep outbox never refuses an append
//! ([ADR-0137](../../../../docs/adr/0137-a-deep-outbox-warns-and-never-refuses.md)).
//!
//! The writer refused the append after the ten-thousandth unsent event, which at a busy store is
//! less than a day offline — and the till saw a `503` on every sale after it. This drives the store
//! past that old ceiling in the batches an edge actually writes and asks for every event back.

#![allow(
    clippy::expect_used,
    reason = "test scaffolding: a failed temp dir, runtime, or writer reply is an unrecoverable fault"
)]

use core::num::NonZeroU32;

use pos_ports::Transactional;
use pos_ports::TxContext;
use pos_ports::event_store::{EventStore, OutboxPosition};
use pos_proto::envelope::{EventEnvelope, EventTypeRef, RawPayload};
use pos_proto::events::EventType;
use pos_proto::ids::{BrandId, DeviceId, EventId, StoreId, TenantId};
use pos_proto::time::{BusinessDate, Timestamp};
use pos_proto::ulid::Ulid;
use store_sqlite::SqliteStore;

/// The ceiling the writer used to refuse at, and one batch past it.
const OLD_CEILING: u128 = 10_000;
const BATCH: u128 = 500;

fn store_id() -> StoreId {
    StoreId::new(Ulid::from_u128(0xB0B))
}

fn envelope(seed: u128) -> EventEnvelope<RawPayload> {
    EventEnvelope {
        event_id: EventId::new(Ulid::from_u128(seed)),
        event_type: EventTypeRef::from_known(EventType::BillingBillSettled),
        event_time: Timestamp::from_milliseconds_since_epoch(1_767_225_600_000).expect("instant"),
        business_date: BusinessDate::from_ymd(2026, 9, 23).expect("date"),
        schema_version: 1,
        tenant_id: TenantId::new(Ulid::from_u128(1)),
        brand_id: BrandId::new(Ulid::from_u128(2)),
        store_id: store_id(),
        device_id: DeviceId::new(Ulid::from_u128(3)),
        employee_id: None,
        shift_id: None,
        chain: None,
        data: RawPayload::encode(&serde_json::json!({ "amount_minor": 150_000 })).expect("payload"),
    }
}

#[tokio::test]
async fn an_outbox_past_the_old_ceiling_still_takes_every_sale() {
    let dir = tempfile::tempdir().expect("temp dir");
    let store = SqliteStore::open(dir.path().join("store.db")).expect("open");

    let total = OLD_CEILING + BATCH;
    let mut next = 1_u128;
    while next <= total {
        let upto = (next + BATCH).min(total + 1);
        let batch: Vec<_> = (next..upto).map(envelope).collect();
        let mut tx = store.begin().await.expect("begin");
        store
            .append(&mut tx, &batch)
            .await
            .expect("a deep outbox must not refuse the append (ADR-0137)");
        tx.commit().await.expect("commit");
        next = upto;
    }

    let depth = store.outbox_depth(store_id()).await.expect("depth");
    assert_eq!(
        u128::from(depth),
        total,
        "every committed event waits for the cloud"
    );

    // And the oldest is still the first one out: nothing was dropped to make room.
    let first = store
        .outbox_batch(store_id(), OutboxPosition::START, NonZeroU32::MIN)
        .await
        .expect("read the outbox");
    assert_eq!(
        first.first().map(|record| record.envelope.event_id),
        Some(EventId::new(Ulid::from_u128(1)))
    );
}
