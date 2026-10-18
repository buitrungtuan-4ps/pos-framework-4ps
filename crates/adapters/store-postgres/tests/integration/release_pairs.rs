// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! A release's pairs: the release id, the report, and the failure a row carries (ADR-0125).

use store_postgres::NewScheduledPublishRow;

use super::{TENANT_A, block_on, prepared};

const STORE: &str = "store-1";
const RELEASE: &str = "01RELEASEAAAAAAAAAAAAAAAAA";

fn pair<'a>(
    id: &'a str,
    node_key: &'a str,
    release_id: Option<&'a str>,
) -> NewScheduledPublishRow<'a> {
    NewScheduledPublishRow {
        id,
        tenant_id: TENANT_A,
        store_id: STORE,
        node_key,
        node_value_json: "{}",
        effective_at_ms: 1_000,
        created_by: "admin-1",
        release_id,
        // The standalone-schedule shape: an instant, resolved against no store's clock.
        resolved_timezone: None,
    }
}

/// The report reads every pair of one release and nothing else, and a row that fails carries
/// its reason until it succeeds.
///
/// Both halves are what a release's `partial` is *derived* from
/// ([ADR-0125](../../../../../docs/adr/0125-a-release-is-one-decision-many-writes.md) §4): a failure
/// only the log can see is a release that reports itself as still trying, and a reason that
/// outlived its failure is a release that looks broken after it recovered.
#[test]
fn a_release_reports_its_own_pairs_and_a_failure_clears_when_it_lands() {
    block_on(async {
        let (store, _admin) = prepared().await.expect("prepare the database");
        let scheduled = store.scheduled_publishes();

        scheduled
            .schedule(&pair("rp-tax", "tax", Some(RELEASE)))
            .await
            .expect("tax");
        scheduled
            .schedule(&pair("rp-menu", "menu", Some(RELEASE)))
            .await
            .expect("menu");
        // A standalone schedule in the same store and tenant: ADR-0077's shape, which must not
        // appear in any release's report.
        scheduled
            .schedule(&pair("rp-solo", "campaigns", None))
            .await
            .expect("solo");

        let report = scheduled
            .list_for_release(TENANT_A, RELEASE)
            .await
            .expect("report");
        assert_eq!(report.len(), 2, "the standalone row is not in the release");
        assert!(
            report
                .iter()
                .all(|row| row.release_id.as_deref() == Some(RELEASE)),
            "every reported pair belongs to the release"
        );
        // Ordered by store then node, so the grid reads the same on every load.
        let nodes: Vec<&str> = report.iter().map(|row| row.node_key.as_str()).collect();
        assert_eq!(nodes, vec!["menu", "tax"]);

        // A failure lands on the row and the row stays pending, to be retried.
        scheduled
            .mark_failed("rp-menu", "the store's configuration changed")
            .await
            .expect("mark failed");
        let after_failure = scheduled
            .list_for_release(TENANT_A, RELEASE)
            .await
            .expect("report after failure");
        let menu = after_failure
            .iter()
            .find(|row| row.id == "rp-menu")
            .expect("the menu pair");
        assert_eq!(menu.status, "PENDING", "a failure is not terminal");
        assert!(
            menu.failure
                .as_deref()
                .is_some_and(|said| said.contains("configuration changed")),
            "the reason is on the row: {:?}",
            menu.failure
        );

        // The retry succeeds, and the row stops explaining itself.
        scheduled
            .mark_applied("rp-menu", "ver-9")
            .await
            .expect("apply");
        let settled = scheduled
            .list_for_release(TENANT_A, RELEASE)
            .await
            .expect("report after apply");
        let menu = settled
            .iter()
            .find(|row| row.id == "rp-menu")
            .expect("the menu pair");
        assert_eq!(menu.status, "APPLIED");
        assert_eq!(
            menu.failure, None,
            "a row that recovered must not keep reporting the failure it survived"
        );
        assert_eq!(menu.applied_version_id.as_deref(), Some("ver-9"));
    });
}
