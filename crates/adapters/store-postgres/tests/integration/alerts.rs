// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! Operational alerts: open→resolved lifecycle, partial-unique dedup (ADR-0073, Track O2).

use super::{block_on, prepared};

/// The full lifecycle: open (tenant-scoped and server-wide), refresh in place (the partial unique
/// index dedups the one *open* alert per key), acknowledge, resolve (drops from active, stays in
/// recent), and reopen the same key past a resolved row.
#[test]
#[expect(
    clippy::too_many_lines,
    reason = "one end-to-end lifecycle: it opens a tenant-scoped and a server-wide alert, \
              refreshes each in place through the partial-unique dedup, acknowledges, resolves \
              (drops from active, stays in recent), and reopens the same key past the resolved \
              row — splitting it would duplicate the multi-alert setup"
)]
fn upserts_refreshes_resolves_and_lists_alerts() {
    block_on(async {
        let (store, admin) = prepared().await.expect("prepare the database");
        let alerts = store.alerts();

        // A tenant-scoped store-offline alert and a server-wide (NULL tenant) JetStream alert.
        alerts
            .upsert(
                "alert-1",
                Some("tenant-a"),
                "store_offline",
                "store-x",
                "warning",
                "offline 6m",
                r#"{"minutes_offline":6}"#,
                1000,
                1000,
            )
            .await
            .expect("open the store alert");
        alerts
            .upsert(
                "alert-2",
                None,
                "jetstream_capacity",
                "",
                "critical",
                "at 85%",
                r#"{"threshold_percent":80}"#,
                1000,
                1000,
            )
            .await
            .expect("open the server-wide alert");
        assert_eq!(alerts.list_active().await.expect("active").len(), 2);

        // A second upsert of the same (tenant, kind, dedup_key) refreshes in place: still two
        // rows, the original id and first_seen kept, last_seen and detail advanced.
        alerts
            .upsert(
                "alert-3",
                Some("tenant-a"),
                "store_offline",
                "store-x",
                "warning",
                "offline 12m",
                r#"{"minutes_offline":12}"#,
                9999,
                5000,
            )
            .await
            .expect("refresh the store alert");
        let count: i64 = admin
            .query_one("SELECT count(*) FROM alerts", &[])
            .await
            .expect("count")
            .get(0);
        assert_eq!(count, 2, "a refresh does not duplicate");
        let active = alerts.list_active().await.expect("active");
        let store_alert = active
            .iter()
            .find(|a| a.kind == "store_offline")
            .expect("the store alert");
        assert_eq!(
            store_alert.id, "alert-1",
            "the original id is kept on refresh"
        );
        assert_eq!(store_alert.first_seen_at_ms, 1000, "first_seen is kept");
        assert_eq!(store_alert.last_seen_at_ms, 5000, "last_seen advanced");

        // Acknowledge keeps it active; resolve drops it from the active list but keeps it in the
        // recent history with both timestamps.
        alerts
            .acknowledge("alert-1", 6000)
            .await
            .expect("acknowledge");
        alerts.resolve("alert-1", 7000).await.expect("resolve");
        let active = alerts.list_active().await.expect("active after resolve");
        assert_eq!(active.len(), 1, "the resolved alert leaves the active list");
        assert_eq!(
            active.first().expect("the remaining active alert").kind,
            "jetstream_capacity"
        );
        let recent = alerts.list_recent(10).await.expect("recent");
        assert_eq!(recent.len(), 2, "the resolved alert stays in history");
        let resolved = recent
            .iter()
            .find(|a| a.id == "alert-1")
            .expect("the resolved alert");
        assert_eq!(resolved.resolved_at_ms, Some(7000));
        assert_eq!(resolved.acknowledged_at_ms, Some(6000));

        // The same condition can open a fresh alert past the resolved one — the partial unique
        // index constrains only unresolved rows.
        alerts
            .upsert(
                "alert-4",
                Some("tenant-a"),
                "store_offline",
                "store-x",
                "warning",
                "offline again",
                "{}",
                12000,
                12000,
            )
            .await
            .expect("reopen the same key");
        let count: i64 = admin
            .query_one("SELECT count(*) FROM alerts", &[])
            .await
            .expect("count")
            .get(0);
        assert_eq!(
            count, 3,
            "a resolved alert does not block a fresh open of the same key"
        );
        assert_eq!(alerts.list_active().await.expect("active").len(), 2);
    });
}

/// The newest *resolved* alert of one kind per scope: what a kind that never clears by itself reads
/// to tell "a person closed this" from "this is new" (ADR-0073 amendment 2026-10-31).
#[test]
fn lists_the_last_resolved_alert_of_a_kind_per_scope() {
    block_on(async {
        let (store, _admin) = prepared().await.expect("prepare the database");
        let alerts = store.alerts();
        let open = |id: &'static str, dedup: &'static str, kind: &'static str, at: i64| {
            let alerts = alerts.clone();
            async move {
                alerts
                    .upsert(
                        id,
                        Some("tenant-a"),
                        kind,
                        dedup,
                        "critical",
                        "broken",
                        r#"{"latest_noticed_at_ms":1}"#,
                        at,
                        at,
                    )
                    .await
                    .expect("open");
            }
        };
        // Store x: closed twice; the later closure is the one returned.
        open("x-1", "store-x", "chain_broken", 1000).await;
        alerts.resolve("x-1", 2000).await.expect("close x-1");
        open("x-2", "store-x", "chain_broken", 3000).await;
        alerts.resolve("x-2", 4000).await.expect("close x-2");
        // Store y: still open, so it has no closed alert to return.
        open("y-1", "store-y", "chain_broken", 1000).await;
        // Another kind, closed: not returned for this one.
        open("o-1", "store-x", "store_offline", 1000).await;
        alerts.resolve("o-1", 5000).await.expect("close o-1");

        let closed = alerts
            .list_last_resolved("chain_broken")
            .await
            .expect("list the closed chain alerts");
        assert_eq!(
            closed.len(),
            1,
            "one per scope, open ones excluded: {closed:?}"
        );
        let last = closed.first().expect("store x's closed chain alert");
        assert_eq!(last.id, "x-2");
        assert_eq!(last.resolved_at_ms, Some(4000));
    });
}
