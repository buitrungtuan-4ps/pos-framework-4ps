// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The item master, paged: window, total, order and the index that carries it (ADR-0098).

use core::fmt::Write as _;

use store_postgres::{ItemOrder, PostgresCatalog};

use super::{TENANT_A, TENANT_B, block_on, prepared};

/// Inserts `count` items for one tenant. The id carries the tenant because `menu_item_id` is the
/// primary key across every tenant: two calls with the same index range would collide.
async fn stock(catalog: &PostgresCatalog, tenant: &str, count: u32) {
    for index in 0..count {
        catalog
            .insert_item(
                &format!("{tenant}-item-{index:04}"),
                tenant,
                &format!("Item {index:04}"),
                "{}",
                "tax-standard",
                None,
                None,
                None,
                None,
            )
            .await
            .expect("insert an item");
    }
}

/// An item's per-locale names survive a create and an update against real PostgreSQL.
///
/// This is a regression guard for a bug these paging tests found, not a paging test. Both writes
/// bind `name_translations` as text into a `jsonb` column, and they cast it `$N::text::jsonb`
/// rather than `$N::jsonb`. The difference is not cosmetic: PostgreSQL infers a bare `$N::jsonb`
/// parameter *as* `jsonb`, and `tokio-postgres` then refuses to send a Rust `&str` for it — so
/// both statements failed at the driver, and every item create and rename answered `503`.
///
/// It shipped because nothing in this suite had ever called `insert_item`: the console tests run
/// against the fake, which has no parameter types to get wrong, and this file only exercised
/// placements. Every other `jsonb` parameter in the tree already carried the double cast; these
/// two, added with the per-locale names column (migration 0029), did not.
#[test]
fn an_items_per_locale_names_round_trip_through_a_create_and_an_update() {
    block_on(async {
        let (store, _admin) = prepared().await.expect("prepare the database");
        let catalog = store.catalog();

        let version = catalog
            .insert_item(
                "item-jsonb",
                TENANT_A,
                "Margherita",
                r#"{"vi":"Bánh Margherita"}"#,
                "tax-standard",
                None,
                None,
                None,
                None,
            )
            .await
            .expect("a create must reach the database, not fail at the driver");

        let stored = catalog.fetch_items(TENANT_A).await.expect("read back");
        let row = stored.first().expect("the item");
        assert_eq!(row.name, "Margherita");
        assert!(
            row.name_translations.contains("Bánh Margherita"),
            "the per-locale name round-trips through the jsonb column; got {:?}",
            row.name_translations,
        );

        let outcome = catalog
            .set_item(
                TENANT_A,
                "item-jsonb",
                "Margherita Classic",
                r#"{"vi":"Bánh Margherita cổ điển"}"#,
                "tax-standard",
                None,
                None,
                None,
                None,
                "active",
                &version,
            )
            .await
            .expect("an update must reach the database too");
        assert!(
            matches!(outcome, store_postgres::RowUpdate::Updated(_)),
            "the update applies at the version the create returned",
        );

        let stored = catalog
            .fetch_items(TENANT_A)
            .await
            .expect("read back again");
        let row = stored.first().expect("the item");
        assert_eq!(row.name, "Margherita Classic");
        assert!(row.name_translations.contains("cổ điển"));
    });
}

/// A page carries its own window and the size of the whole item master, and consecutive pages
/// partition it without overlap or gaps (ADR-0098 slice B3-2).
#[test]
fn a_page_of_the_item_master_carries_the_window_and_the_total() {
    block_on(async {
        let (store, _admin) = prepared().await.expect("prepare the database");
        let catalog = store.catalog();
        stock(&catalog, TENANT_A, 25).await;
        // A neighbour's items must reach neither the page nor the count.
        stock(&catalog, TENANT_B, 7).await;

        let (first, total) = catalog
            .fetch_items_page(TENANT_A, None, ItemOrder::Newest, false, 10, 0)
            .await
            .expect("first page");
        assert_eq!(first.len(), 10, "the window is the limit");
        assert_eq!(total, 25, "the total is the master, not the window");

        let mut seen: Vec<String> = first.into_iter().map(|row| row.menu_item_id).collect();
        for offset in [10, 20] {
            let (page, page_total) = catalog
                .fetch_items_page(TENANT_A, None, ItemOrder::Newest, false, 10, offset)
                .await
                .expect("later page");
            assert_eq!(page_total, 25, "the total does not change as pages advance");
            seen.extend(page.into_iter().map(|row| row.menu_item_id));
        }
        assert_eq!(seen.len(), 25, "three pages of ten cover twenty-five items");
        let mut unique = seen.clone();
        unique.sort_unstable();
        unique.dedup();
        assert_eq!(unique.len(), 25, "no item appears on two pages");
        assert!(
            unique.iter().all(|id| id.starts_with(TENANT_A)),
            "no neighbour's item reached the pages",
        );
    });
}

/// The paged read returns the same rows in the same order as the whole-set read, and a page past
/// the end is empty rather than an error.
#[test]
fn the_paged_item_master_agrees_with_the_whole_set_read() {
    block_on(async {
        let (store, _admin) = prepared().await.expect("prepare the database");
        let catalog = store.catalog();
        stock(&catalog, TENANT_A, 6).await;
        stock(&catalog, TENANT_B, 4).await;

        let whole: Vec<String> = catalog
            .fetch_items(TENANT_A)
            .await
            .expect("whole set")
            .into_iter()
            .map(|row| row.menu_item_id)
            .collect();
        assert_eq!(whole.len(), 6, "only this tenant's items");

        let (paged, total) = catalog
            .fetch_items_page(TENANT_A, None, ItemOrder::Newest, false, 6, 0)
            .await
            .expect("paged");
        assert_eq!(total, 6, "the count is tenant-scoped too");
        let paged: Vec<String> = paged.into_iter().map(|row| row.menu_item_id).collect();
        assert_eq!(
            paged, whole,
            "a full-width page is the whole-set read, in the same order"
        );

        // A page past the end: empty, but `total` still reports the whole master. The window
        // count rides on the rows and there are none, so the adapter falls back to a second
        // count rather than claiming the tenant has no items.
        let (beyond, beyond_total) = catalog
            .fetch_items_page(TENANT_A, None, ItemOrder::Newest, false, 10, 100)
            .await
            .expect("a page past the end still reads");
        assert!(beyond.is_empty());
        assert_eq!(
            beyond_total, 6,
            "an empty window still reports the size of the set it is past the end of"
        );
    });
}

/// A batch written in one transaction shares one `created_at`, which is why the read needs the
/// `menu_item_id` tiebreaker — the premise of ADR-0098 decision 9, measured rather than trusted.
///
/// A CSV import of a menu is exactly this shape. The tie assertion is deterministic; the
/// partition check below it is the property that follows, and the `EXPLAIN` guard is what
/// actually catches the tiebreaker going missing.
#[test]
fn an_imported_batch_shares_one_timestamp_and_still_pages() {
    block_on(async {
        let (store, admin) = prepared().await.expect("prepare the database");
        let catalog = store.catalog();
        let mut inserts = String::new();
        for index in 0..6 {
            write!(
                inserts,
                "INSERT INTO catalog_items \
                 (menu_item_id, tenant_id, name, tax_class_id) \
                 VALUES ('import-{index:04}', '{TENANT_A}', 'Imported {index}', 'tax-standard');"
            )
            .expect("writing to a String cannot fail");
        }
        admin
            .batch_execute(&format!("BEGIN; {inserts} COMMIT;"))
            .await
            .expect("write the import in one transaction");

        let distinct: i64 = admin
            .query_one(
                "SELECT count(DISTINCT created_at) FROM catalog_items WHERE tenant_id = $1",
                &[&TENANT_A],
            )
            .await
            .expect("count the distinct timestamps")
            .get(0);
        assert_eq!(
            distinct, 1,
            "six items imported in one transaction carry one created_at, not six close ones — \
             which is why the read's ORDER BY needs menu_item_id as a tiebreaker"
        );

        let mut seen = Vec::new();
        for offset in [0, 2, 4] {
            let (page, total) = catalog
                .fetch_items_page(TENANT_A, None, ItemOrder::Newest, false, 2, offset)
                .await
                .expect("page over the imported batch");
            assert_eq!(total, 6);
            seen.extend(page.into_iter().map(|row| row.menu_item_id));
        }
        let mut unique = seen.clone();
        unique.sort_unstable();
        unique.dedup();
        assert_eq!(
            unique.len(),
            6,
            "three pages of two cover the batch exactly once each; got {seen:?}"
        );
    });
}

/// `?q=` narrows the rows and the total together, and reaches the per-locale names.
///
/// Only real PostgreSQL can answer this: the predicate is `position(lower(..) in lower(..))` over
/// `name` *and* over every value in a `jsonb` document via `jsonb_each_text`, with the count
/// riding on the same statement. A `Vec::filter` fake agreeing with itself proves none of it.
#[test]
fn a_search_narrows_the_page_and_its_total_including_per_locale_names() {
    block_on(async {
        let (store, _admin) = prepared().await.expect("prepare the database");
        let catalog = store.catalog();
        for (id, name, vi) in [
            ("i-1", "Margherita", "Banh Margherita"),
            ("i-2", "Marinara", "Banh Marinara"),
            ("i-3", "Tiramisu", "Banh ngot Tiramisu"),
        ] {
            catalog
                .insert_item(
                    id,
                    TENANT_A,
                    name,
                    &format!("{{\"vi\":\"{vi}\"}}"),
                    "tax-standard",
                    None,
                    None,
                    None,
                    None,
                )
                .await
                .expect("insert an item");
        }

        // A primary-name substring, case-insensitively.
        let (rows, total) = catalog
            .fetch_items_page(TENANT_A, Some("MARI"), ItemOrder::Newest, false, 10, 0)
            .await
            .expect("search");
        assert_eq!(total, 1, "the total counts the match, not the master");
        assert_eq!(rows.first().expect("a row").name, "Marinara");

        // A per-locale substring appearing in no primary name at all.
        let (rows, total) = catalog
            .fetch_items_page(TENANT_A, Some("ngot"), ItemOrder::Newest, false, 10, 0)
            .await
            .expect("search");
        assert_eq!(total, 1, "the per-locale name is searched too");
        assert_eq!(rows.first().expect("a row").name, "Tiramisu");

        // No match is an empty page and a zero total, not an error — and this zero is the true
        // one. The empty-window fallback runs here too, carrying the same search predicate, so
        // it counts the match (none) rather than the master (six).
        let (rows, total) = catalog
            .fetch_items_page(TENANT_A, Some("carbonara"), ItemOrder::Newest, false, 10, 0)
            .await
            .expect("search");
        assert!(rows.is_empty());
        assert_eq!(
            total, 0,
            "the fallback count is filtered, not the whole master"
        );

        // `%` is a character in the needle, not a wildcard: the read uses `position`, not
        // `ILIKE`, so an operator searching for a literal percent gets a literal search.
        let (rows, _total) = catalog
            .fetch_items_page(TENANT_A, Some("%"), ItemOrder::Newest, false, 10, 0)
            .await
            .expect("search");
        assert!(
            rows.is_empty(),
            "a percent sign matches nothing here; under ILIKE it would match everything",
        );
    });
}

/// Each order the route offers is total in SQL, so pages partition the set under every one.
///
/// Every item here shares a name, so `?sort=name` rests entirely on the tiebreaker — the case
/// that would silently repeat or skip a row if the `ORDER BY` stopped at `name`.
#[test]
fn every_offered_order_partitions_the_master_in_both_directions() {
    block_on(async {
        let (store, _admin) = prepared().await.expect("prepare the database");
        let catalog = store.catalog();
        for index in 0..6 {
            catalog
                .insert_item(
                    &format!("same-{index:04}"),
                    TENANT_A,
                    "Margherita",
                    "{}",
                    "tax-standard",
                    None,
                    None,
                    None,
                    None,
                )
                .await
                .expect("insert an item");
        }

        for order in [ItemOrder::Newest, ItemOrder::Name, ItemOrder::Status] {
            for descending in [false, true] {
                let mut seen = Vec::new();
                for offset in [0, 2, 4] {
                    let (page, total) = catalog
                        .fetch_items_page(TENANT_A, None, order, descending, 2, offset)
                        .await
                        .expect("page");
                    assert_eq!(total, 6);
                    seen.extend(page.into_iter().map(|row| row.menu_item_id));
                }
                let mut unique = seen.clone();
                unique.sort_unstable();
                unique.dedup();
                assert_eq!(
                    unique.len(),
                    6,
                    "{order:?} descending={descending} covers each item once; got {seen:?}",
                );
            }
        }
    });
}

/// Migration 0044's index carries the *name* order, so `?sort=name` stops a scan rather than
/// sorting the whole master — the economy 0043 bought for the default order, kept for the second.
///
/// This is the guard that makes offering a second order safe: without it, a query parameter
/// reintroduces exactly the cost 0043 exists to remove.
#[test]
fn the_name_sorted_page_is_served_by_its_own_index() {
    block_on(async {
        let (store, admin) = prepared().await.expect("prepare the database");
        let catalog = store.catalog();
        stock(&catalog, TENANT_A, 40).await;
        admin
            .batch_execute("ANALYZE catalog_items")
            .await
            .expect("analyze");

        let plan = {
            admin
                .batch_execute("SET enable_seqscan = off")
                .await
                .expect("prefer an index if one fits");
            let rows = admin
                .query(
                    "EXPLAIN SELECT menu_item_id FROM catalog_items WHERE tenant_id = $1 \
                     ORDER BY name ASC, menu_item_id ASC LIMIT $2 OFFSET $3",
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
            plan.contains("catalog_items_by_tenant_name"),
            "the name sort should be served by migration 0044's index; plan was:\n{plan}"
        );
        assert!(
            !plan.contains("Sort Key"),
            "an index that carries the order needs no sort step; plan was:\n{plan}"
        );
    });
}

/// Migration 0043's index carries the read's whole order, so `LIMIT` stops the scan instead of
/// truncating a sort of every item a chain sells.
#[test]
fn the_paged_item_query_is_served_by_the_index_and_not_by_a_sort() {
    block_on(async {
        let (store, admin) = prepared().await.expect("prepare the database");
        let catalog = store.catalog();
        stock(&catalog, TENANT_A, 40).await;
        admin
            .batch_execute("ANALYZE catalog_items")
            .await
            .expect("analyze");

        let plan = {
            admin
                .batch_execute("SET enable_seqscan = off")
                .await
                .expect("prefer an index if one fits");
            let rows = admin
                .query(
                    "EXPLAIN SELECT menu_item_id FROM catalog_items WHERE tenant_id = $1 \
                     ORDER BY created_at DESC, menu_item_id DESC LIMIT $2 OFFSET $3",
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
            plan.contains("catalog_items_by_tenant_newest"),
            "the page should be served by the sort-carrying index; plan was:\n{plan}"
        );
        assert!(
            !plan.contains("Sort Key"),
            "an index that carries the order needs no sort step; plan was:\n{plan}"
        );
    });
}
