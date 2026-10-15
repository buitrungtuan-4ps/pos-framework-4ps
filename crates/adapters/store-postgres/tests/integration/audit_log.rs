// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The console audit trail: append-only, tenant-scoped, INSERT/SELECT-only (ADR-0069).

use super::{block_on, prepared};
use store_postgres::AuditOrder;

/// Entries append and read back newest-first, scoped to their tenant (a NULL-tenant global entry
/// shows only in the fleet-wide read); before/after round-trip through jsonb; and the grant is
/// append-only — `app_tenant` has SELECT/INSERT but not UPDATE or DELETE ([ADR-0069]).
#[test]
#[expect(
    clippy::too_many_lines,
    reason = "one end-to-end scenario: it appends three entries (tenant-scoped and global), \
              reads them back scoped and fleet-wide, checks the jsonb round-trip, and asserts \
              the append-only grant — splitting it would duplicate the multi-row setup"
)]
fn appends_scoped_newest_first_and_is_append_only() {
    block_on(async {
        let (store, admin) = prepared().await.expect("prepare the database");
        let audit = store.audit();

        // Two entries for tenant-a (an update, then an archive carrying before/after), and one
        // tenant-global entry (NULL tenant) for a tenant create.
        audit
            .insert(
                "01AUDIT000000000000000001A",
                Some("tenant-a"),
                "01ADMIN0000000000000000OPS",
                "ops@pizza4ps.test",
                "ops",
                "store.update",
                "store",
                "store-1",
                None,
                Some(r#"{"name":"Bến Thành"}"#),
                None,
                1000,
            )
            .await
            .expect("append the first entry");
        audit
            .insert(
                "01AUDIT000000000000000002A",
                Some("tenant-a"),
                "01ADMIN0000000000000000OPS",
                "ops@pizza4ps.test",
                "ops",
                "store.archive",
                "store",
                "store-1",
                Some(r#"{"name":"Bến Thành","status":"active"}"#),
                Some(r#"{"name":"Bến Thành","status":"archived"}"#),
                Some("req-9"),
                2000,
            )
            .await
            .expect("append the second entry");
        audit
            .insert(
                "01AUDIT000000000000000003G",
                None,
                "01ADMIN00000000000000OWNER",
                "owner@pizza4ps.test",
                "owner",
                "tenant.create",
                "tenant",
                "tenant-a",
                None,
                Some(r#"{"name":"Pizza 4P's"}"#),
                None,
                3000,
            )
            .await
            .expect("append the global entry");

        // Tenant-scoped read: only tenant-a's two, newest-first.
        let scoped = audit
            .fetch(Some("tenant-a"), 10)
            .await
            .expect("fetch tenant-a");
        assert_eq!(scoped.len(), 2, "only the tenant's own entries");
        let newest = scoped.first().expect("an entry");
        assert_eq!(newest.action, "store.archive", "newest-first");
        assert_eq!(newest.at_ms, 2000);
        assert_eq!(newest.request_id.as_deref(), Some("req-9"));
        let after: serde_json::Value =
            serde_json::from_str(newest.after_json.as_deref().expect("an after value"))
                .expect("after is valid json");
        assert_eq!(
            after.get("status").and_then(serde_json::Value::as_str),
            Some("archived"),
            "before/after round-trip through jsonb"
        );

        // Fleet-wide read: all three, including the NULL-tenant global entry.
        let all = audit.fetch(None, 10).await.expect("fetch all");
        assert_eq!(all.len(), 3, "the fleet-wide read spans every tenant");
        assert!(
            all.iter()
                .any(|row| row.tenant_id.is_none() && row.action == "tenant.create"),
            "the tenant-global entry is included"
        );

        // A different tenant sees nothing.
        let other = audit
            .fetch(Some("tenant-b"), 10)
            .await
            .expect("fetch tenant-b");
        assert!(other.is_empty(), "the read is scoped to the tenant");

        // Append-only at the grant: app_tenant may SELECT/INSERT but never UPDATE or DELETE.
        let can_update: bool = admin
            .query_one(
                "SELECT has_table_privilege('app_tenant', 'audit_log', 'UPDATE')",
                &[],
            )
            .await
            .expect("privilege check")
            .get(0);
        let can_delete: bool = admin
            .query_one(
                "SELECT has_table_privilege('app_tenant', 'audit_log', 'DELETE')",
                &[],
            )
            .await
            .expect("privilege check")
            .get(0);
        assert!(
            !can_update,
            "audit_log is append-only: no UPDATE for app_tenant"
        );
        assert!(
            !can_delete,
            "audit_log is append-only: no DELETE for app_tenant"
        );
    });
}

/// `search` applies each non-`None` filter in SQL before the limit: by entity type, by action,
/// by acting admin, and by a time window — so a narrow filter reaches the matching rows and a
/// tenant filter still excludes the tenant-global `NULL` rows ([ADR-0069] slice 4).
#[test]
#[expect(
    clippy::too_many_lines,
    reason = "one scenario: it seeds four rows across tenants/actors/times then exercises each \
              filter dimension (entity, action, actor, time window, tenant) in turn; splitting \
              it would duplicate the shared multi-row setup"
)]
fn search_filters_by_entity_action_actor_and_time() {
    block_on(async {
        let (store, _admin) = prepared().await.expect("prepare the database");
        let audit = store.audit();
        let seed = |id: &'static str,
                    tenant: Option<&'static str>,
                    actor: &'static str,
                    action: &'static str,
                    entity_type: &'static str,
                    at_ms: i64| {
            let audit = audit.clone();
            async move {
                audit
                    .insert(
                        id,
                        tenant,
                        actor,
                        "a@pizza4ps.test",
                        "ops",
                        action,
                        entity_type,
                        "entity-1",
                        None,
                        None,
                        None,
                        at_ms,
                    )
                    .await
                    .expect("append");
            }
        };
        seed(
            "01SRCH00000000000000000S1",
            Some("tenant-a"),
            "admin-x",
            "store.update",
            "store",
            1000,
        )
        .await;
        seed(
            "01SRCH00000000000000000S2",
            Some("tenant-a"),
            "admin-y",
            "store.create",
            "store",
            2000,
        )
        .await;
        seed(
            "01SRCH00000000000000000M1",
            Some("tenant-a"),
            "admin-x",
            "menu.create",
            "menu",
            3000,
        )
        .await;
        seed(
            "01SRCH00000000000000000G1",
            None,
            "admin-x",
            "tenant.create",
            "tenant",
            4000,
        )
        .await;

        // By entity type: only the two store rows, newest first.
        let stores = audit
            .search(
                Some("tenant-a"),
                Some("store"),
                None,
                None,
                None,
                None,
                None,
                10,
            )
            .await
            .expect("search by entity type");
        assert_eq!(stores.len(), 2);
        assert_eq!(
            stores.first().expect("a row").action,
            "store.create",
            "newest first"
        );

        // By action.
        let creates = audit
            .search(None, None, None, Some("store.create"), None, None, None, 10)
            .await
            .expect("search by action");
        assert_eq!(creates.len(), 1);
        assert_eq!(
            creates.first().expect("a row").id,
            "01SRCH00000000000000000S2"
        );

        // By acting admin, fleet-wide (includes the NULL-tenant global row).
        let by_x = audit
            .search(None, None, None, None, Some("admin-x"), None, None, 10)
            .await
            .expect("search by actor");
        assert_eq!(by_x.len(), 3, "admin-x acted three times across tenants");
        assert!(
            by_x.iter().any(|row| row.tenant_id.is_none()),
            "the fleet-wide read includes the global row"
        );

        // By time window [1500, 3500]: the create + the menu row.
        let window = audit
            .search(
                Some("tenant-a"),
                None,
                None,
                None,
                None,
                Some(1500),
                Some(3500),
                10,
            )
            .await
            .expect("search by time window");
        assert_eq!(window.len(), 2);
        assert!(
            window
                .iter()
                .all(|row| row.at_ms >= 1500 && row.at_ms <= 3500),
            "only rows inside the window"
        );

        // A tenant filter excludes the tenant-global row.
        let scoped = audit
            .search(Some("tenant-a"), None, None, None, None, None, None, 10)
            .await
            .expect("search scoped");
        assert_eq!(
            scoped.len(),
            3,
            "the three tenant-a rows, not the global one"
        );
        assert!(
            scoped
                .iter()
                .all(|row| row.tenant_id.as_deref() == Some("tenant-a")),
            "a tenant filter never returns NULL-tenant rows"
        );
    });
}

/// Appends `count` entries for one tenant, one per millisecond so the order is unambiguous
/// before the `id` tiebreaker is even consulted.
///
/// The id carries the tenant because `audit_log.id` is the primary key across every tenant: two
/// calls with the same index range would collide, not partition.
async fn trail(audit: &store_postgres::PostgresAudit, tenant: &str, count: i64) {
    for index in 0..count {
        audit
            .insert(
                &format!("{tenant}-page-{index:04}"),
                Some(tenant),
                "01ADMIN0000000000000000OPS",
                "ops@pizza4ps.test",
                "ops",
                "store.update",
                "store",
                "store-1",
                None,
                None,
                None,
                1_000 + index,
            )
            .await
            .expect("append an entry");
    }
}

/// A page of the trail carries its own window and the size of the *matching* set, and
/// consecutive pages partition that set (ADR-0098 slice B3-2).
///
/// `total` counting the filtered match rather than the whole log is the property that matters
/// here: a pager over a filtered view whose total came from the unfiltered table would offer
/// pages that are empty.
#[test]
fn a_page_of_the_trail_carries_the_window_and_the_matching_total() {
    block_on(async {
        let (store, _admin) = prepared().await.expect("prepare the database");
        let audit = store.audit();
        trail(&audit, "tenant-a", 25).await;
        // A neighbour's entries, and one tenant-global row, must reach neither the page nor the
        // count when a tenant is named.
        trail(&audit, "tenant-b", 7).await;

        let (first, total) = audit
            .search_page(
                Some("tenant-a"),
                None,
                None,
                None,
                None,
                None,
                None,
                AuditOrder::Newest,
                10,
                0,
            )
            .await
            .expect("first page");
        assert_eq!(first.len(), 10, "the window is the limit");
        assert_eq!(total, 25, "the total is the matching set, not the window");

        let mut seen: Vec<String> = first.into_iter().map(|row| row.id).collect();
        for offset in [10, 20] {
            let (page, page_total) = audit
                .search_page(
                    Some("tenant-a"),
                    None,
                    None,
                    None,
                    None,
                    None,
                    None,
                    AuditOrder::Newest,
                    10,
                    offset,
                )
                .await
                .expect("later page");
            assert_eq!(page_total, 25, "the total does not change as pages advance");
            seen.extend(page.into_iter().map(|row| row.id));
        }
        assert_eq!(
            seen.len(),
            25,
            "three pages of ten cover twenty-five entries"
        );
        let mut unique = seen.clone();
        unique.sort_unstable();
        unique.dedup();
        assert_eq!(unique.len(), 25, "no entry appears on two pages");

        // A narrower filter narrows the total too, not just the rows.
        let (rows, filtered_total) = audit
            .search_page(
                Some("tenant-a"),
                None,
                None,
                None,
                None,
                Some(1_005),
                Some(1_009),
                AuditOrder::Newest,
                10,
                0,
            )
            .await
            .expect("filtered page");
        assert_eq!(filtered_total, 5, "five entries fall inside the window");
        assert_eq!(rows.len(), 5);
    });
}

/// The paged read matches the same rows in the same order as the windowed one, and a page past
/// the end is empty rather than an error.
#[test]
fn the_paged_trail_agrees_with_the_windowed_read_on_order_and_scope() {
    block_on(async {
        let (store, _admin) = prepared().await.expect("prepare the database");
        let audit = store.audit();
        trail(&audit, "tenant-a", 6).await;
        trail(&audit, "tenant-b", 4).await;

        let windowed: Vec<String> = audit
            .search(Some("tenant-a"), None, None, None, None, None, None, 6)
            .await
            .expect("windowed")
            .into_iter()
            .map(|row| row.id)
            .collect();
        assert_eq!(windowed.len(), 6, "only this tenant's entries");

        let (paged, total) = audit
            .search_page(
                Some("tenant-a"),
                None,
                None,
                None,
                None,
                None,
                None,
                AuditOrder::Newest,
                6,
                0,
            )
            .await
            .expect("paged");
        assert_eq!(total, 6, "the count is tenant-scoped too");
        let paged: Vec<String> = paged.into_iter().map(|row| row.id).collect();
        assert_eq!(
            paged, windowed,
            "a full-width page is the windowed read, in the same order"
        );

        // A page past the end: empty, but `total` still counts what the filters matched. The
        // window count rides on the rows and there are none, so the adapter falls back to a
        // second count carrying the same predicates.
        let (beyond, beyond_total) = audit
            .search_page(
                Some("tenant-a"),
                None,
                None,
                None,
                None,
                None,
                None,
                AuditOrder::Newest,
                10,
                100,
            )
            .await
            .expect("a page past the end still reads");
        assert!(beyond.is_empty());
        assert_eq!(
            beyond_total, 6,
            "an empty window still reports how many rows the filters matched"
        );
    });
}

/// Every page of `tenant-a`'s trail in one order, stitched into the sequence a caller paging
/// through would see, ten at a time.
///
/// Reads until a page comes back empty rather than dividing by `expected`, so a page that
/// dropped or repeated a row lands in the stitched sequence instead of being hidden by the
/// arithmetic. `expected` is checked against the window count on every non-empty page.
async fn stitched_trail(
    audit: &store_postgres::PostgresAudit,
    order: AuditOrder,
    expected: i64,
) -> Vec<String> {
    let mut ids: Vec<String> = Vec::new();
    let mut offset = 0;
    loop {
        let (page, total) = audit
            .search_page(
                Some("tenant-a"),
                None,
                None,
                None,
                None,
                None,
                None,
                order,
                10,
                offset,
            )
            .await
            .expect("a page of the trail");
        if page.is_empty() {
            return ids;
        }
        assert_eq!(
            total, expected,
            "{order:?}: the order does not change how many matched",
        );
        ids.extend(page.into_iter().map(|row| row.id));
        offset += 10;
    }
}

/// The reversed order windows the same set backwards, and the same index still serves it.
///
/// Two properties in one scenario because they are one claim: `?order=oldest` shipped without a
/// migration on the grounds that `audit_log_by_tenant_newest` read *backwards* is the whole of
/// the oldest-first order. The set assertion proves the pages are a partition; the plan
/// assertion proves the database gets there by walking that index rather than by sorting.
#[test]
fn the_reversed_trail_partitions_the_same_set_and_is_still_served_by_the_index() {
    block_on(async {
        let (store, admin) = prepared().await.expect("prepare the database");
        let audit = store.audit();
        trail(&audit, "tenant-a", 25).await;
        trail(&audit, "tenant-b", 7).await;

        let newest = stitched_trail(&audit, AuditOrder::Newest, 25).await;
        let oldest = stitched_trail(&audit, AuditOrder::Oldest, 25).await;
        let mut reversed = oldest.clone();
        reversed.reverse();
        assert_eq!(
            reversed, newest,
            "one order is the other read backwards — every page of it, not one page flipped",
        );
        let mut unique = oldest.clone();
        unique.sort_unstable();
        unique.dedup();
        assert_eq!(unique.len(), 25, "no entry appears on two pages either way");

        admin
            .batch_execute("ANALYZE audit_log")
            .await
            .expect("analyze");
        let plan = {
            admin
                .batch_execute("SET enable_seqscan = off")
                .await
                .expect("prefer an index if one fits");
            let rows = admin
                .query(
                    "EXPLAIN SELECT id FROM audit_log WHERE tenant_id = $1 \
                     ORDER BY at ASC, id ASC LIMIT $2 OFFSET $3",
                    &[&"tenant-a", &10_i64, &0_i64],
                )
                .await
                .expect("explain");
            admin
                .batch_execute("RESET enable_seqscan")
                .await
                .expect("restore");
            rows.iter()
                .map(|row| row.get::<_, String>(0))
                .collect::<Vec<_>>()
                .join("\n")
        };
        assert!(
            plan.contains("audit_log_by_tenant_newest"),
            "the newest-first index should serve the reversed order too; plan was:\n{plan}"
        );
        assert!(
            !plan.contains("Sort Key"),
            "reading that index backwards needs no sort step; plan was:\n{plan}"
        );
    });
}

/// Migration 0042's index carries the trail's whole order, tiebreaker included, so a page needs
/// no sort step above the scan.
///
/// `audit_log`'s read was already ordered totally (`at DESC, id DESC`) — this is the one paged
/// table where nothing about the query changed. What was missing was the `id` column in the
/// index, without which the plan finishes the order with a `Sort` and `LIMIT` truncates a
/// completed sort. That is also what makes the `count(*) OVER()` walk index-only.
#[test]
fn the_paged_trail_query_is_served_by_the_index_and_not_by_a_sort() {
    block_on(async {
        let (store, admin) = prepared().await.expect("prepare the database");
        let audit = store.audit();
        trail(&audit, "tenant-a", 40).await;
        admin
            .batch_execute("ANALYZE audit_log")
            .await
            .expect("analyze");

        let plan = {
            admin
                .batch_execute("SET enable_seqscan = off")
                .await
                .expect("prefer an index if one fits");
            let rows = admin
                .query(
                    "EXPLAIN SELECT id FROM audit_log WHERE tenant_id = $1 \
                     ORDER BY at DESC, id DESC LIMIT $2 OFFSET $3",
                    &[&"tenant-a", &10_i64, &0_i64],
                )
                .await
                .expect("explain");
            admin
                .batch_execute("RESET enable_seqscan")
                .await
                .expect("restore");
            rows.iter()
                .map(|row| row.get::<_, String>(0))
                .collect::<Vec<_>>()
                .join("\n")
        };
        assert!(
            plan.contains("audit_log_by_tenant_newest"),
            "the page should be served by the sort-carrying index; plan was:\n{plan}"
        );
        assert!(
            !plan.contains("Sort Key"),
            "an index that carries the order needs no sort step; plan was:\n{plan}"
        );
    });
}
