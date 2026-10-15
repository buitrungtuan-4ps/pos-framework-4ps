// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! Media renditions: bytea round-trip, single-rendition read, tenant isolation, delete (ADR-0075).

use core::fmt::Write as _;

use super::{TENANT_A, TENANT_B, block_on, prepared};

/// Stores `count` assets for one tenant, each in its own transaction, with ids that sort the
/// same way whichever column the read leans on.
async fn upload(media: &store_postgres::PostgresMedia, tenant: &str, count: u32) {
    for index in 0..count {
        media
            .insert(
                &format!("{tenant}-asset-{index:04}"),
                tenant,
                "image/jpeg",
                &[0x01],
                &[0x02, 0x03],
                2,
            )
            .await
            .expect("insert an asset");
    }
}

#[test]
fn stores_reads_one_rendition_lists_and_stays_tenant_scoped() {
    block_on(async {
        let (store, _admin) = prepared().await.expect("prepare the database");
        let media = store.media();

        let thumbnail = vec![0xFFu8, 0xD8, 0xFF, 0x00, 0x01];
        let detail = vec![0xFFu8, 0xD8, 0xFF, 0x10, 0x11, 0x12, 0x13];
        media
            .insert(
                "asset-1",
                TENANT_A,
                "image/jpeg",
                &thumbnail,
                &detail,
                i32::try_from(detail.len()).expect("fits"),
            )
            .await
            .expect("insert ours");
        // A neighbour tenant owns a different asset. Media ids are globally-unique ULIDs in
        // production (a minted id is never shared across tenants — `media_id` is the table's primary
        // key), so isolation is what the `tenant_id` predicate on every read enforces: a tenant
        // cannot reach another's asset by id, and a listing shows only its own.
        media
            .insert("asset-2", TENANT_B, "image/jpeg", &[0x01], &[0x02], 1)
            .await
            .expect("insert neighbour");

        // Each rendition round-trips its exact bytes.
        assert_eq!(
            media
                .fetch_rendition(TENANT_A, "asset-1", false)
                .await
                .expect("thumbnail"),
            Some(thumbnail),
        );
        assert_eq!(
            media
                .fetch_rendition(TENANT_A, "asset-1", true)
                .await
                .expect("detail"),
            Some(detail.clone()),
        );

        // A listing shows the size without the bytes, tenant-scoped.
        let listed = media.fetch_summaries(TENANT_A).await.expect("list ours");
        assert_eq!(listed.len(), 1);
        let row = listed.first().expect("row");
        assert_eq!(row.media_id, "asset-1");
        assert_eq!(row.content_type, "image/jpeg");
        assert_eq!(
            usize::try_from(row.detail_bytes).expect("non-negative"),
            detail.len()
        );

        // A tenant cannot read another tenant's asset by id — the read is `None`, not a leak —
        // while the neighbour reads its own, and neither can reach across.
        assert_eq!(
            media
                .fetch_rendition(TENANT_B, "asset-1", true)
                .await
                .expect("neighbour read"),
            None,
            "a tenant cannot read another tenant's asset by id"
        );
        assert_eq!(
            media
                .fetch_rendition(TENANT_B, "asset-2", true)
                .await
                .expect("neighbour detail"),
            Some(vec![0x02]),
            "the neighbour reads its own asset"
        );
        assert_eq!(
            media
                .fetch_rendition(TENANT_A, "asset-2", true)
                .await
                .expect("cross read"),
            None,
            "and cannot reach the neighbour's asset"
        );

        // Delete removes only the named asset and reports it.
        assert!(media.remove(TENANT_A, "asset-1").await.expect("remove"));
        assert!(
            !media.remove(TENANT_A, "asset-1").await.expect("remove"),
            "removing an absent asset reports no row removed"
        );
        assert!(
            media
                .fetch_summaries(TENANT_A)
                .await
                .expect("list")
                .is_empty()
        );
        assert_eq!(
            media
                .fetch_summaries(TENANT_B)
                .await
                .expect("neighbour")
                .len(),
            1,
            "the neighbour's asset is untouched"
        );
    });
}

/// A page carries its own window and the size of the whole library, and consecutive pages
/// partition that library without overlap or gaps (ADR-0098 slice B3-2).
///
/// This is the half only real PostgreSQL can answer: `count(*) OVER()` is a window function
/// whose value depends on the frame the planner builds, and `LIMIT`/`OFFSET` interact with the
/// `ORDER BY` in ways a `Vec::skip().take()` fake cannot reproduce.
#[test]
fn a_page_of_the_library_carries_the_window_and_the_total_and_pages_do_not_overlap() {
    block_on(async {
        let (store, _admin) = prepared().await.expect("prepare the database");
        let media = store.media();
        upload(&media, TENANT_A, 25).await;

        let (first, total) = media
            .fetch_summaries_page(TENANT_A, 10, 0)
            .await
            .expect("first page");
        assert_eq!(first.len(), 10, "the window is the limit");
        assert_eq!(total, 25, "the total is the library, not the window");

        let mut seen: Vec<String> = first.into_iter().map(|row| row.media_id).collect();
        for offset in [10, 20] {
            let (page, page_total) = media
                .fetch_summaries_page(TENANT_A, 10, offset)
                .await
                .expect("later page");
            assert_eq!(page_total, 25, "the total does not change as pages advance");
            seen.extend(page.into_iter().map(|row| row.media_id));
        }
        assert_eq!(
            seen.len(),
            25,
            "three pages of ten cover twenty-five assets"
        );
        let mut unique = seen.clone();
        unique.sort_unstable();
        unique.dedup();
        assert_eq!(unique.len(), 25, "no asset appears on two pages");
    });
}

/// The paged read orders the same way the unpaged one does, is tenant-scoped the same way, and
/// answers a page past the end with no rows rather than an error.
#[test]
fn the_paged_library_agrees_with_the_unpaged_one_on_order_and_scope() {
    block_on(async {
        let (store, _admin) = prepared().await.expect("prepare the database");
        let media = store.media();
        upload(&media, TENANT_A, 6).await;
        // A neighbour's assets must reach neither read nor the count.
        upload(&media, TENANT_B, 4).await;

        let unpaged: Vec<String> = media
            .fetch_summaries(TENANT_A)
            .await
            .expect("unpaged")
            .into_iter()
            .map(|row| row.media_id)
            .collect();
        assert_eq!(unpaged.len(), 6, "only this tenant's assets");

        let (paged, total) = media
            .fetch_summaries_page(TENANT_A, 6, 0)
            .await
            .expect("paged");
        assert_eq!(total, 6, "the count is tenant-scoped too");
        let paged: Vec<String> = paged.into_iter().map(|row| row.media_id).collect();
        assert_eq!(
            paged, unpaged,
            "a full-width page is the unpaged read, in the same order"
        );

        // A page past the end: empty, but `total` still reports the whole library. The window
        // count rides on the rows and there are none, so the adapter falls back to a second
        // count rather than claiming the tenant has no assets.
        let (beyond, beyond_total) = media
            .fetch_summaries_page(TENANT_A, 10, 100)
            .await
            .expect("a page past the end still reads");
        assert!(beyond.is_empty());
        assert_eq!(
            beyond_total, 6,
            "an empty window still reports the size of the set it is past the end of"
        );
    });
}

/// The premise ADR-0098 decision 9 rests on, measured here rather than trusted: `created_at`
/// defaults to `now()`, which is *transaction* time, so assets written in one transaction share
/// one timestamp exactly — and `created_at DESC` alone therefore orders nothing across them.
///
/// The assertion on the tie is deterministic and is the point of the test. The partition check
/// below it is the property that follows, but note honestly what it does and does not prove:
/// with the tiebreaker dropped, PostgreSQL *may* return a stable order anyway for a given plan,
/// so this test alone would not reliably catch that. What catches it is the pair — this test
/// pins the premise, and `the_paged_library_query_is_served_by_the_index_and_not_by_a_sort`
/// pins the index that carries the total order.
#[test]
fn a_batch_written_in_one_transaction_shares_one_timestamp_and_still_pages() {
    block_on(async {
        let (store, admin) = prepared().await.expect("prepare the database");
        let media = store.media();
        // One transaction, six rows — the shape a bulk import or a seeded fixture produces.
        let mut inserts = String::new();
        for index in 0..6 {
            write!(
                inserts,
                "INSERT INTO media_assets \
                 (media_id, tenant_id, content_type, thumbnail, detail, detail_bytes) \
                 VALUES ('batch-{index:04}', '{TENANT_A}', 'image/jpeg', \
                 '\\x01'::bytea, '\\x0203'::bytea, 2);"
            )
            .expect("writing to a String cannot fail");
        }
        admin
            .batch_execute(&format!("BEGIN; {inserts} COMMIT;"))
            .await
            .expect("write the batch in one transaction");

        let distinct: i64 = admin
            .query_one(
                "SELECT count(DISTINCT created_at) FROM media_assets WHERE tenant_id = $1",
                &[&TENANT_A],
            )
            .await
            .expect("count the distinct timestamps")
            .get(0);
        assert_eq!(
            distinct, 1,
            "six rows in one transaction carry one created_at, not six close ones — \
             which is why the read's ORDER BY needs media_id as a tiebreaker"
        );

        let mut seen = Vec::new();
        for offset in [0, 2, 4] {
            let (page, total) = media
                .fetch_summaries_page(TENANT_A, 2, offset)
                .await
                .expect("page over the tied batch");
            assert_eq!(total, 6);
            seen.extend(page.into_iter().map(|row| row.media_id));
        }
        let mut unique = seen.clone();
        unique.sort_unstable();
        unique.dedup();
        assert_eq!(
            unique.len(),
            6,
            "three pages of two cover the tied batch exactly once each; got {seen:?}"
        );
    });
}

/// The index migration 0041 added covers this query's `ORDER BY`, so `LIMIT` stops the scan
/// instead of truncating a sort of the whole library.
///
/// Asserted through `EXPLAIN` rather than by timing: a timing test on 40 rows would pass either
/// way. What this catches is the migration being dropped or the read's `ORDER BY` drifting away
/// from the index it was built for — including the tiebreaker, whose absence from the index is
/// what would put a `Sort` node back above the scan.
#[test]
fn the_paged_library_query_is_served_by_the_index_and_not_by_a_sort() {
    block_on(async {
        let (store, admin) = prepared().await.expect("prepare the database");
        let media = store.media();
        upload(&media, TENANT_A, 40).await;
        // Without statistics the planner picks a sequential scan on a table this small
        // whatever indexes exist, so the assertion would be about row count rather than about
        // the index. `ANALYZE` plus a disabled seqscan asks the question the test means.
        admin
            .batch_execute("ANALYZE media_assets")
            .await
            .expect("analyze");

        let plan = {
            admin
                .batch_execute("SET enable_seqscan = off")
                .await
                .expect("prefer an index if one fits");
            let rows = admin
                .query(
                    "EXPLAIN SELECT media_id FROM media_assets WHERE tenant_id = $1 \
                     ORDER BY created_at DESC, media_id DESC LIMIT $2 OFFSET $3",
                    &[&TENANT_A, &10_i64, &0_i64],
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
            plan.contains("media_assets_by_tenant_newest"),
            "the page should be served by the sort-carrying index; plan was:\n{plan}"
        );
        assert!(
            !plan.contains("Sort Key"),
            "an index that carries the order needs no sort step; plan was:\n{plan}"
        );
    });
}
