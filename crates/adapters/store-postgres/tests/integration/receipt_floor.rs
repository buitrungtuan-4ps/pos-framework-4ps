// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The receipt floor a lease bump publishes (ADR-0149).

use super::{block_on, prepared};
use pos_ports::{EventStore, Transactional, TxContext};
use pos_proto::envelope::{EventTypeRef, RawPayload};
use pos_proto::events::EventType;
use pos_proto::{StoreId, Ulid};

/// The highest receipt number is read from the store's own settled bills: other stores and
/// other event types do not count, and a store with no settled bill has none.
#[test]
fn the_highest_receipt_comes_from_this_store_s_settled_bills() {
    block_on(async {
        let (store, _admin) = prepared().await.expect("prepare the database");
        let this = StoreId::new(Ulid::from_u128(0xF100));
        let other = StoreId::new(Ulid::from_u128(0xF200));
        let trees = store.config_trees();
        assert_eq!(
            trees.highest_receipt_number(this).await.expect("read"),
            None,
            "no settled bill, no floor"
        );

        let settled = |store_id: StoreId, seed: u32, receipt: u64| {
            let mut event = pos_contract_tests::fixtures::activation(store_id, seed);
            event.event_type = EventTypeRef::from_known(EventType::BillingBillSettled);
            event.data = RawPayload::encode(&serde_json::json!({ "receipt_number": receipt }))
                .expect("a payload");
            event
        };
        // One store per batch, as every append is.
        let batches = [
            vec![
                settled(this, 1, 7),
                settled(this, 2, 41),
                settled(this, 3, 12),
                // An activation event is not a bill, whatever its data says.
                pos_contract_tests::fixtures::activation(this, 5),
            ],
            vec![settled(other, 4, 900)],
        ];
        for batch in &batches {
            let mut tx = store.begin().await.expect("begin");
            store.append(&mut tx, batch).await.expect("append");
            tx.commit().await.expect("commit");
        }

        assert_eq!(
            trees.highest_receipt_number(this).await.expect("read"),
            Some(41)
        );
        assert_eq!(
            trees.highest_receipt_number(other).await.expect("read"),
            Some(900)
        );
    });
}
