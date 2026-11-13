// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The gateway's idempotency record (ADR-0198 decision 6, migration 0099).

use super::{TENANT_A, TENANT_B, block_on, prepared};

use store_postgres::GatewayCallRow;

const STORE: &str = "01J0000000000000000000STR1";
const DAY_MS: i64 = 24 * 60 * 60 * 1_000;

fn call(endpoint: &str, points: u32) -> GatewayCallRow {
    GatewayCallRow {
        gateway_endpoint_id: endpoint.to_owned(),
        answer_json: serde_json::json!({ "call_id": "C", "outcome": "GATEWAY_OUTCOME_ANSWERED", "result": { "points": points } })
            .to_string(),
    }
}

/// A key is recalled for a day and no longer; a younger answer is never replaced, an older one is;
/// keeping a call forgets the store's day-old keys; another tenant reads none of it.
#[test]
fn a_kept_answer_lives_one_day_and_is_never_overwritten_inside_it() {
    block_on(async {
        let (store, _admin) = prepared().await.expect("prepare the database");
        let calls = store.gateway_calls();
        let at = 1_800_000_000_000_i64;

        calls
            .keep(TENANT_A, STORE, "k1", &call("E1", 1), at, at - DAY_MS)
            .await
            .expect("keep");
        let recalled = calls
            .recall(TENANT_A, STORE, "k1", at + 10 - DAY_MS)
            .await
            .expect("recall")
            .expect("kept");
        assert_eq!(recalled.gateway_endpoint_id, "E1");
        assert!(recalled.answer_json.contains(r#""points": 1"#));
        assert!(
            calls
                .recall(TENANT_B, STORE, "k1", at - DAY_MS)
                .await
                .expect("recall")
                .is_none(),
            "another tenant reads nothing"
        );

        // Inside the day, a second keep does not replace the first answer.
        calls
            .keep(
                TENANT_A,
                STORE,
                "k1",
                &call("E1", 2),
                at + 5,
                at + 5 - DAY_MS,
            )
            .await
            .expect("keep again");
        let still = calls
            .recall(TENANT_A, STORE, "k1", at - DAY_MS)
            .await
            .expect("recall")
            .expect("kept");
        assert!(still.answer_json.contains(r#""points": 1"#), "{still:?}");

        // A day on, the key is forgotten, and keeping another call deletes it.
        let later = at + DAY_MS + 1;
        assert!(
            calls
                .recall(TENANT_A, STORE, "k1", later - DAY_MS)
                .await
                .expect("recall")
                .is_none()
        );
        calls
            .keep(TENANT_A, STORE, "k2", &call("E1", 3), later, later - DAY_MS)
            .await
            .expect("keep k2");
        assert!(
            calls
                .recall(TENANT_A, STORE, "k1", 0)
                .await
                .expect("recall at any age")
                .is_none(),
            "the day-old row is gone"
        );
    });
}
