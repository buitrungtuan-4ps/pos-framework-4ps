// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The reconciliation membership query (ADR-0040): which candidate ids the
//! cloud already holds, scoped by tenant and store.

use super::{TENANT_A, block_on, prepared, seed_row};

/// The event id `seed_row` writes for `seed`.
fn evt(seed: i32) -> String {
    format!("EVT{seed:022}")
}

#[test]
fn present_event_ids_answers_membership_scoped_by_tenant_and_store() {
    block_on(async {
        let (store, admin) = prepared().await.expect("prepare the database");
        // Two events for (TENANT_A, store-1); one for a different store; one for a different
        // tenant — none of which must count toward store-1's membership.
        seed_row(&admin, 1, TENANT_A, "store-1")
            .await
            .expect("seed 1");
        seed_row(&admin, 2, TENANT_A, "store-1")
            .await
            .expect("seed 2");
        seed_row(&admin, 3, TENANT_A, "store-2")
            .await
            .expect("seed 3");
        seed_row(&admin, 4, "tenant-b", "store-1")
            .await
            .expect("seed 4");

        let reconcile = store.reconcile();
        // Candidates: 1 and 2 are present for store-1; 3 belongs to store-2; 9 was never written.
        let candidates = vec![evt(1), evt(2), evt(3), evt(9)];
        let present = reconcile
            .present_event_ids(TENANT_A, "store-1", &candidates)
            .await
            .expect("membership query");
        let mut present = present;
        present.sort();
        assert_eq!(
            present,
            vec![evt(1), evt(2)],
            "only ids actually in (TENANT_A, store-1) count — not store-2's, not tenant-b's, \
             and not one that was never written"
        );

        // An empty candidate set is an empty answer and touches nothing.
        let none = reconcile
            .present_event_ids(TENANT_A, "store-1", &[])
            .await
            .expect("empty query");
        assert!(none.is_empty(), "no candidates, no membership");
    });
}

#[test]
fn records_and_lists_reconciliation_runs_scoped_by_tenant_and_store() {
    block_on(async {
        let (store, _admin) = prepared().await.expect("prepare the database");
        let reconcile = store.reconcile();

        // Two runs for (TENANT_A, store-1) at different instants, one for store-2, and one for a
        // different tenant — the last two must not leak into TENANT_A's history.
        reconcile
            .record_reconcile_run("run-1", TENANT_A, "store-1", 10, 3, 1_000)
            .await
            .expect("record run 1");
        reconcile
            .record_reconcile_run("run-2", TENANT_A, "store-1", 8, 0, 2_000)
            .await
            .expect("record run 2");
        reconcile
            .record_reconcile_run("run-3", TENANT_A, "store-2", 5, 5, 1_500)
            .await
            .expect("record run 3");
        reconcile
            .record_reconcile_run("run-4", "tenant-b", "store-1", 9, 9, 3_000)
            .await
            .expect("record run 4");

        // Tenant-wide, newest first: store-1's two runs and store-2's one, ordered by `ran_at`.
        let all = reconcile
            .list_reconcile_runs(TENANT_A, None, 10)
            .await
            .expect("list tenant runs");
        let ids: Vec<&str> = all.iter().map(|row| row.run_id.as_str()).collect();
        assert_eq!(
            ids,
            vec!["run-2", "run-3", "run-1"],
            "TENANT_A's runs only, newest first; tenant-b's run-4 is not present"
        );
        let newest = all.first().expect("a newest run");
        assert_eq!(newest.store_id, "store-1");
        assert_eq!(newest.candidates_offered, 8);
        assert_eq!(newest.missing_found, 0);
        assert_eq!(newest.ran_at, 2_000);

        // Narrowed to one store.
        let store_one = reconcile
            .list_reconcile_runs(TENANT_A, Some("store-1"), 10)
            .await
            .expect("list store-1 runs");
        let ids: Vec<&str> = store_one.iter().map(|row| row.run_id.as_str()).collect();
        assert_eq!(ids, vec!["run-2", "run-1"], "only store-1's runs");

        // The limit caps the page.
        let capped = reconcile
            .list_reconcile_runs(TENANT_A, None, 1)
            .await
            .expect("list capped");
        assert_eq!(capped.len(), 1, "the limit caps the page");
        assert_eq!(
            capped.first().expect("a capped run").run_id,
            "run-2",
            "and keeps the newest"
        );
    });
}
