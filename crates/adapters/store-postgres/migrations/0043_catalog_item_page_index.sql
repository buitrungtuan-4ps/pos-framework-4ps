-- Copyright (c) 2026 Pizza 4P's. All rights reserved.
-- Proprietary and confidential. Internal use only. See LICENSE.

-- An index covering the item master's *total* order, so a page of it is both correct and cheap
-- (ADR-0098 decision 9, F2 item B3 slice 2).
--
-- `0012_cloud_catalog.sql` created `catalog_items_by_tenant (tenant_id)`, which finds a tenant's
-- items but carries neither sort column. Unpaged that cost was unavoidable — the caller wanted the
-- whole item master anyway, and five of the six console consumers of this read still do. Paged it
-- would be absurd: `LIMIT 25` would shrink the response while the database sorted every item a
-- chain sells, on every page of the pager.
--
-- The read also gains a tiebreaker in the same change. `created_at` defaults to `now()`, and in
-- PostgreSQL `now()` is *transaction* time: every row one transaction writes carries the identical
-- timestamp, not merely a close one. A CSV import of a menu is one such transaction. `ORDER BY
-- created_at DESC` therefore imposes no order at all across an imported batch, and `LIMIT`/`OFFSET`
-- over a non-total order may return a row on two pages or on neither — the database is free to
-- break the tie differently between the query that builds page one and the query that builds page
-- two. So the read now orders `created_at DESC, menu_item_id DESC`, total since `menu_item_id` is
-- the primary key, and this index carries the whole of it.
--
-- Why a second index rather than replacing the first: `CREATE INDEX` is additive and `DROP INDEX` is
-- not (ADR-0017 is forward-only), and the single-column index still serves the point reads and the
-- menu compiler's whole-set fetch. The overlap costs one index's worth of writes on a table an
-- operator edits by hand.
--
-- Forward-only and additive, applied idempotently on every boot (ADR-0017). No data change.

CREATE INDEX IF NOT EXISTS catalog_items_by_tenant_newest
    ON catalog_items (tenant_id, created_at DESC, menu_item_id DESC);
