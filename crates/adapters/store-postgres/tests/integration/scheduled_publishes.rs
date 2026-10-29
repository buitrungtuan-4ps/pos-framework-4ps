// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! Scheduled publishes: schedule, due-by-time, list-for-store, cancel, mark-applied (ADR-0077).

use store_postgres::NewScheduledPublishRow;

use super::{TENANT_A, block_on, prepared};

const STORE: &str = "store-1";

fn row(id: &str, effective_at_ms: i64) -> NewScheduledPublishRow<'_> {
    NewScheduledPublishRow {
        id,
        tenant_id: TENANT_A,
        store_id: STORE,
        node_key: "campaigns",
        node_value_json: "{\"campaigns\":[]}",
        effective_at_ms,
        created_by: "admin-1",
        release_id: None,
        resolved_timezone: None,
    }
}

/// A schedule inserts pending; `due` returns only ripe pending rows; `list_for_store` sees all
/// pending; `cancel` and `mark_applied` withdraw a row from the pending/due sets.
#[test]
fn schedule_due_list_cancel_and_apply() {
    block_on(async {
        let (store, _admin) = prepared().await.expect("prepare the database");
        let scheduled = store.scheduled_publishes();

        scheduled
            .schedule(&row("sp-past", 1_000))
            .await
            .expect("past");
        scheduled
            .schedule(&row("sp-future", 9_999_999_999))
            .await
            .expect("future");

        // Only the ripe one is due at t=5000.
        let due = scheduled.due(5_000).await.expect("due");
        assert_eq!(due.len(), 1);
        let ripe = due.first().expect("row");
        assert_eq!(ripe.id, "sp-past");
        assert_eq!(ripe.node_key, "campaigns");
        assert_eq!(ripe.status, "PENDING");
        assert!(ripe.node_value_json.contains("campaigns"));

        // Both are pending for the store.
        assert_eq!(
            scheduled
                .list_for_store(TENANT_A, STORE)
                .await
                .expect("list")
                .len(),
            2
        );

        // Applying the past one drops it from due and records the version.
        scheduled
            .mark_applied("sp-past", "ver-1")
            .await
            .expect("apply");
        assert!(
            scheduled
                .due(5_000)
                .await
                .expect("due after apply")
                .is_empty()
        );

        // Cancelling the future one removes it from the store's pending list.
        assert!(
            scheduled
                .cancel(TENANT_A, "sp-future")
                .await
                .expect("cancel")
        );
        assert!(
            scheduled
                .list_for_store(TENANT_A, STORE)
                .await
                .expect("list after cancel")
                .is_empty()
        );
        // Cancelling an unknown id changes nothing.
        assert!(
            !scheduled
                .cancel(TENANT_A, "nope")
                .await
                .expect("cancel nope")
        );
    });
}
