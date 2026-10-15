// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! Background-task health: last-tick per loop, upserted (ADR-0068 slice 4).

use super::{block_on, prepared};

/// A loop's tick records a row; a second tick upserts it in place (one row, latest instant and
/// detail win); and the detail JSON round-trips through the `jsonb` column.
#[test]
fn records_and_upserts_a_loop_tick() {
    block_on(async {
        let (store, admin) = prepared().await.expect("prepare the database");
        let health = store.task_health();

        // First tick: the projector at t=1000ms, folding four stores.
        health
            .record(
                "rollup_projector",
                1000,
                r#"{"ok":true,"interval_secs":30,"folded":4}"#,
            )
            .await
            .expect("record the first tick");
        let rows = health.fetch_all().await.expect("fetch all");
        assert_eq!(rows.len(), 1, "one loop, one row");
        let first = rows.first().expect("the recorded row");
        assert_eq!(first.task, "rollup_projector");
        assert_eq!(first.last_tick_at_ms, 1000);
        let detail: serde_json::Value =
            serde_json::from_str(&first.detail_json).expect("detail is valid json");
        assert_eq!(
            detail.get("folded").and_then(serde_json::Value::as_i64),
            Some(4),
            "the detail round-trips through jsonb"
        );

        // A second tick for the same loop upserts in place: still one row, advanced instant.
        health
            .record(
                "rollup_projector",
                5000,
                r#"{"ok":true,"interval_secs":30,"folded":0}"#,
            )
            .await
            .expect("record the second tick");
        let count: i64 = admin
            .query_one("SELECT count(*) FROM task_health", &[])
            .await
            .expect("count")
            .get(0);
        assert_eq!(count, 1, "the second tick upserts rather than duplicating");
        let rows = health.fetch_all().await.expect("fetch all");
        assert_eq!(
            rows.first().expect("the upserted row").last_tick_at_ms,
            5000,
            "the instant advanced"
        );

        // A second, distinct loop gets its own row; fetch_all orders most-recently-ticked first.
        health
            .record("retention", 9000, r#"{"ok":true,"interval_secs":86400}"#)
            .await
            .expect("record a second loop");
        let rows = health.fetch_all().await.expect("fetch all");
        assert_eq!(rows.len(), 2, "two distinct loops, two rows");
        assert_eq!(
            rows.first().expect("the newest row").task,
            "retention",
            "the most recently ticked loop sorts first"
        );
    });
}
