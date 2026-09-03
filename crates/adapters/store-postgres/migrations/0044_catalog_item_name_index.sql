-- Copyright (c) 2026 Pizza 4P's. All rights reserved.
-- Proprietary and confidential. Internal use only. See LICENSE.

-- An index covering the item master's *name* order, the second order `GET /admin/catalog/items` now
-- offers (ADR-0098 slice B3-3).
--
-- Migration 0043 gave the default order (`created_at DESC, menu_item_id DESC`) an index. `?sort=name`
-- is a different order and needs its own, or the page is a `LIMIT` over a sort of every item a chain
-- sells — the exact cost 0043 exists to avoid, reintroduced by a query parameter.
--
-- The tiebreaker is here for the same reason it is in 0043, and the reason generalises: `ORDER BY
-- name` is no more total than `ORDER BY created_at` was. Two items can share a name — a chain with
-- "Margherita" on both a lunch and a dinner master is ordinary — and `LIMIT`/`OFFSET` over that tie
-- can return a row on two pages or on neither. So the read orders `name, menu_item_id` and this
-- index carries both.
--
-- The column order is ascending because that is the direction the console asks for; PostgreSQL can
-- walk a btree backwards, so the same index serves `?order=desc` without a second one.
--
-- No index for `?sort=status`: `status` holds one of two values, so an index on it narrows nothing a
-- scan of one tenant's items does not, and the sort is over that same small set either way. Stated
-- here so the absence reads as a decision rather than an oversight.
--
-- Forward-only and additive, applied idempotently on every boot (ADR-0017). No data change.

CREATE INDEX IF NOT EXISTS catalog_items_by_tenant_name
    ON catalog_items (tenant_id, name, menu_item_id);
