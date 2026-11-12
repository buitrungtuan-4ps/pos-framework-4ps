-- Copyright (c) 2026 Pizza 4P's. All rights reserved.
-- Proprietary and confidential. Internal use only. See LICENSE.
--
-- 0096 — an area may sit inside another area
-- ([ADR-0211](../../../../docs/adr/0211-an-area-may-sit-inside-another-area.md)).
--
--   * `parent_area_id` — the `floor_areas.id` this area sits inside, in the same tenant and store,
--                        or null for an area at the top. A floor is simply a top-level area.
--
-- Nullable, and the null is the whole compatibility story: every area there is today is at the top,
-- and a store whose areas are all at the top publishes exactly the `floor` node it published before.
-- A soft reference like `floor_tables.area_id` (0025), not a foreign key under RLS. The rules that
-- keep it a tree (no cycle, at most eight levels, a live area's parent is live) are checked by
-- `pos-cloud` inside the write's transaction, with the store's areas locked, because a check
-- constraint cannot see other rows.
--
-- Forward-only and additive, applied idempotently on every boot (ADR-0017).
ALTER TABLE floor_areas ADD COLUMN IF NOT EXISTS parent_area_id text;
CREATE INDEX IF NOT EXISTS floor_areas_by_parent ON floor_areas (tenant_id, parent_area_id);
