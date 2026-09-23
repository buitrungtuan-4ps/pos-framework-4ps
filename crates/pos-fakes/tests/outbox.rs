// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The fake store agrees with the real one: a deep outbox never refuses an append
//! ([ADR-0137](../../../docs/adr/0137-a-deep-outbox-warns-and-never-refuses.md)).
//!
//! The fake used to refuse after the ten-thousandth unsent event *after* storing the event it
//! refused — a state the real store could never reach — and every edge test ran against that rule.

#![allow(
    clippy::expect_used,
    reason = "test scaffolding: a fake that refuses here is the failure under test"
)]

use pos_fakes::FakeStore;
use pos_fakes::executor::run_ready;
use pos_ports::event_store::EventStore;
use pos_ports::{Transactional, TxContext};
use pos_proto::envelope::{EventEnvelope, EventTypeRef, RawPayload};
use pos_proto::events::EventType;
use pos_proto::ids::{BrandId, DeviceId, EventId, StoreId, TenantId};
use pos_proto::time::{BusinessDate, Timestamp};
use pos_proto::ulid::Ulid;

const OLD_CEILING: u128 = 10_000;

fn store_id() -> StoreId {
    StoreId::new(Ulid::from_u128(0xFA4E))
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

#[test]
fn an_outbox_past_the_old_ceiling_still_takes_every_sale() {
    let store = FakeStore::default();
    run_ready(async {
        let batch: Vec<_> = (1..=OLD_CEILING).map(envelope).collect();
        let mut tx = store.begin().await.expect("begin");
        store
            .append(&mut tx, &batch)
            .await
            .expect("append the old ceiling's worth");
        tx.commit().await.expect("commit");

        let mut tx = store.begin().await.expect("begin");
        store
            .append(&mut tx, &[envelope(OLD_CEILING + 1)])
            .await
            .expect("a deep outbox must not refuse the append (ADR-0137)");
        tx.commit().await.expect("commit");

        let depth = store.outbox_depth(store_id()).await.expect("depth");
        assert_eq!(u128::from(depth), OLD_CEILING + 1);
    });
}
