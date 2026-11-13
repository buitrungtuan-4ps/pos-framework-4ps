// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! A relayed order found by the id its store gave it (migration 0102, ADR-0215 decision 4).

use super::{block_on, prepared};

/// The reference a store accepted as one of its orders is found by that order's id, on its channel
/// and for that store only; a pending order, another store's or another channel's is not.
#[test]
fn a_store_s_order_is_found_by_the_id_it_gave_it() {
    block_on(async {
        let (store, _admin) = prepared().await.expect("prepare the database");
        let queue = store.order_queue();
        let (tenant, shop, channel) = (
            "TENANT000000000000000000AA",
            "STORE0000000000000000000AA",
            "SALES_CHANNEL_DELIVERY",
        );
        let payload = serde_json::json!({ "lines": [] });
        for (reference, queued) in [
            ("CONN:D-1", "QUEUED00000000000000000001"),
            ("CONN:D-2", "QUEUED00000000000000000002"),
        ] {
            queue
                .enqueue(tenant, shop, channel, reference, queued, &payload)
                .await
                .expect("enqueue");
        }
        let accepted = serde_json::json!({
            "outcome": "accepted", "order_id": "ORDER0000000000000000000AA", "created": true,
            "total": { "currency_code": "VND", "amount_minor": 1 }, "repriced": false,
            "awaiting_staff_confirmation": true,
        });
        assert!(
            queue
                .record_outcome(tenant, shop, "QUEUED00000000000000000001", &accepted)
                .await
                .expect("report")
        );

        let found = queue
            .reference_of_order(tenant, shop, channel, "ORDER0000000000000000000AA")
            .await
            .expect("look up");
        assert_eq!(found.as_deref(), Some("CONN:D-1"));
        for (tenant_id, store_id, sales_channel, order_id) in [
            (
                tenant,
                "STORE0000000000000000000BB",
                channel,
                "ORDER0000000000000000000AA",
            ),
            (
                tenant,
                shop,
                "SALES_CHANNEL_QR",
                "ORDER0000000000000000000AA",
            ),
            (tenant, shop, channel, "ORDER0000000000000000000BB"),
        ] {
            assert_eq!(
                queue
                    .reference_of_order(tenant_id, store_id, sales_channel, order_id)
                    .await
                    .expect("look up"),
                None
            );
        }
    });
}
