// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! Voucher instances: batch insert, code uniqueness per tenant, list-by-campaign, isolation (ADR-0077).

use store_postgres::NewVoucherRow;

use super::{TENANT_A, TENANT_B, block_on, prepared};

/// A batch inserts atomically, lists by campaign newest-first, rejects a duplicate code within the
/// tenant, and lets a neighbour tenant reuse the same code (codes are unique per tenant).
#[test]
fn mint_batch_list_and_code_uniqueness() {
    block_on(async {
        let (store, _admin) = prepared().await.expect("prepare the database");
        let vouchers = store.vouchers();

        let batch = [
            NewVoucherRow {
                voucher_id: "v-1",
                campaign_id: "camp-1",
                code: "ALPHA",
            },
            NewVoucherRow {
                voucher_id: "v-2",
                campaign_id: "camp-1",
                code: "BRAVO",
            },
        ];
        vouchers.insert_batch(TENANT_A, &batch).await.expect("mint");

        let listed = vouchers
            .list_by_campaign(TENANT_A, "camp-1")
            .await
            .expect("list");
        assert_eq!(listed.len(), 2);
        assert!(listed.iter().all(|row| row.status == "ACTIVE"));
        assert!(listed.iter().any(|row| row.code == "ALPHA"));

        // A neighbour tenant may reuse the same code — codes are unique per tenant, not globally.
        vouchers
            .insert_batch(
                TENANT_B,
                &[NewVoucherRow {
                    voucher_id: "v-1",
                    campaign_id: "camp-1",
                    code: "ALPHA",
                }],
            )
            .await
            .expect("neighbour reuse");

        // A duplicate code within the tenant violates the unique constraint and fails the batch.
        let collision = vouchers
            .insert_batch(
                TENANT_A,
                &[NewVoucherRow {
                    voucher_id: "v-9",
                    campaign_id: "camp-1",
                    code: "ALPHA",
                }],
            )
            .await;
        assert!(collision.is_err(), "a duplicate code is rejected");

        // A different campaign in the same tenant lists separately.
        assert!(
            vouchers
                .list_by_campaign(TENANT_A, "camp-2")
                .await
                .expect("list other")
                .is_empty()
        );
    });
}

/// Mints `count` codes for one campaign of one tenant.
///
/// Codes are `CODE0000`-style so they sort predictably, and the ids likewise, because the read's
/// order is `created_at DESC, voucher_id DESC` and a batch inserted in one statement shares a
/// `created_at` — so `voucher_id` is what actually breaks the tie, and a test that could not tell
/// the two apart would pass on a query that dropped the second sort key.
async fn mint(
    vouchers: &store_postgres::PostgresVouchers,
    tenant: &str,
    campaign: &str,
    count: u32,
) {
    let ids: Vec<(String, String)> = (0..count)
        .map(|n| (format!("v-{n:04}"), format!("CODE{n:04}")))
        .collect();
    let batch: Vec<NewVoucherRow<'_>> = ids
        .iter()
        .map(|(voucher_id, code)| NewVoucherRow {
            voucher_id,
            campaign_id: campaign,
            code,
        })
        .collect();
    vouchers
        .insert_batch(tenant, &batch)
        .await
        .expect("mint the batch");
}

/// A page carries its own window and the size of the whole set, and consecutive pages partition
/// that set without overlap or gaps (ADR-0098 slice B3-1).
///
/// This is the half only real PostgreSQL can answer: `count(*) OVER()` is a window function whose
/// value depends on the frame the planner builds, and `LIMIT`/`OFFSET` interact with the
/// `ORDER BY` in ways a `Vec::skip().take()` fake cannot reproduce. A fake agreeing with itself
/// proves nothing about the SQL.
#[test]
fn a_page_carries_the_window_and_the_total_and_pages_do_not_overlap() {
    block_on(async {
        let (store, _admin) = prepared().await.expect("prepare the database");
        let vouchers = store.vouchers();
        mint(&vouchers, TENANT_A, "camp-1", 25).await;

        let (first, total) = vouchers
            .list_by_campaign_page(TENANT_A, "camp-1", 10, 0)
            .await
            .expect("first page");
        assert_eq!(first.len(), 10, "the window is the limit");
        assert_eq!(total, 25, "the total is the set, not the window");

        let mut seen: Vec<String> = first.into_iter().map(|row| row.code).collect();
        for offset in [10, 20] {
            let (page, page_total) = vouchers
                .list_by_campaign_page(TENANT_A, "camp-1", 10, offset)
                .await
                .expect("later page");
            assert_eq!(page_total, 25, "the total does not change as pages advance");
            seen.extend(page.into_iter().map(|row| row.code));
        }
        assert_eq!(seen.len(), 25, "three pages of ten cover twenty-five rows");
        let mut unique = seen.clone();
        unique.sort_unstable();
        unique.dedup();
        assert_eq!(unique.len(), 25, "no code appears on two pages");
    });
}

/// The paged read orders the same way the unpaged one does, is tenant-scoped the same way, and
/// answers a page past the end with no rows rather than an error.
#[test]
fn the_paged_read_agrees_with_the_unpaged_one_on_order_and_scope() {
    block_on(async {
        let (store, _admin) = prepared().await.expect("prepare the database");
        let vouchers = store.vouchers();
        mint(&vouchers, TENANT_A, "camp-1", 6).await;
        // A neighbour's codes for the same campaign id must not reach either read.
        mint(&vouchers, TENANT_B, "camp-1", 4).await;

        let unpaged: Vec<String> = vouchers
            .list_by_campaign(TENANT_A, "camp-1")
            .await
            .expect("unpaged")
            .into_iter()
            .map(|row| row.code)
            .collect();
        assert_eq!(unpaged.len(), 6, "only this tenant's codes");

        let (paged, total) = vouchers
            .list_by_campaign_page(TENANT_A, "camp-1", 6, 0)
            .await
            .expect("paged");
        assert_eq!(total, 6, "the count is tenant-scoped too");
        let paged: Vec<String> = paged.into_iter().map(|row| row.code).collect();
        assert_eq!(
            paged, unpaged,
            "a full-width page is the unpaged read, in the same order"
        );

        // A page past the end of the set: empty, but `total` still reports the whole set. The
        // window count rides on the rows and there are none, so the adapter falls back to a
        // second count — without it the pager would be told the campaign has no codes and
        // would offer no page to go back to.
        let (beyond, beyond_total) = vouchers
            .list_by_campaign_page(TENANT_A, "camp-1", 10, 100)
            .await
            .expect("a page past the end still reads");
        assert!(beyond.is_empty());
        assert_eq!(
            beyond_total, 6,
            "an empty window still reports the size of the set it is past the end of"
        );
    });
}

/// The index migration 0040 added covers this query's `ORDER BY`, so `LIMIT` stops the scan
/// instead of truncating a sort of every matching row.
///
/// Asserted through `EXPLAIN` rather than by timing, because a timing test on 25 rows would pass
/// either way and a timing test large enough to fail would be slow and flaky. What this catches
/// is the migration being dropped or the query's `ORDER BY` drifting away from the index it was
/// built for — at which point the plan grows a `Sort` node and paging becomes a page-shaped scan.
#[test]
fn the_paged_query_is_served_by_the_index_and_not_by_a_sort() {
    block_on(async {
        let (store, admin) = prepared().await.expect("prepare the database");
        let vouchers = store.vouchers();
        mint(&vouchers, TENANT_A, "camp-1", 40).await;
        // Without statistics the planner will pick a sequential scan on any table this small
        // regardless of the index, so the assertion would be about row count rather than about
        // the index. `ANALYZE` plus a disabled seqscan asks the question the test means: given
        // that it must use an index, is there one that already carries this order?
        admin
            .batch_execute("ANALYZE vouchers")
            .await
            .expect("analyze");

        // `EXPLAIN` through the admin client rather than a method on the adapter: a plan probe
        // is a test's business, and adding one to the production surface to satisfy a test is
        // how a diagnostic becomes an API.
        let plan = {
            admin
                .batch_execute("SET enable_seqscan = off")
                .await
                .expect("prefer an index if one fits");
            let rows = admin
                .query(
                    "EXPLAIN SELECT voucher_id FROM vouchers \
                     WHERE tenant_id = $1 AND campaign_id = $2 \
                     ORDER BY created_at DESC, voucher_id DESC LIMIT $3 OFFSET $4",
                    &[&TENANT_A, &"camp-1", &10_i64, &0_i64],
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
            plan.contains("vouchers_by_campaign_newest"),
            "the page should be served by the sort-carrying index; plan was:\n{plan}"
        );
        assert!(
            !plan.contains("Sort Key"),
            "an index that carries the order needs no sort step; plan was:\n{plan}"
        );
    });
}
