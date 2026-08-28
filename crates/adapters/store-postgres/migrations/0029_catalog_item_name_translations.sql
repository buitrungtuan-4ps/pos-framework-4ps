-- Copyright (c) 2026 Pizza 4P's. All rights reserved.
-- Proprietary and confidential. Internal use only. See LICENSE.

-- Per-locale item names (Track M4, ADR-0074). An item's `name` (0012) is its default caption and the
-- always-present fallback; this column carries its name in each locale the operator translates it into,
-- so the compiled `MenuEntry` can travel with every language and the store's display language selects
-- one at the edge. Absent a translation for the chosen language, the edge falls back to `name` — the
-- never-blank rule, so an untranslated item shows its default name rather than nothing.
--
-- A single jsonb object (locale code → name), not a side table: the map is small (a handful of
-- languages), read and written whole with the item, and it round-trips in a diff. `NOT NULL DEFAULT
-- '{}'` so an existing row and an item authored without any translations both read as "no
-- translations". The `name` fallback stays required, mirroring the `"en"`-always-present rule the
-- translation grid (0008) already enforces for UI strings.
--
-- Forward-only and additive, applied idempotently on every boot (ADR-0017): `ADD COLUMN IF NOT EXISTS`
-- so re-running is a no-op. Tenant isolation, RLS, and the `app_tenant` grant are inherited from the
-- `catalog_items` table (0012) — a new column on an existing table needs no new policy or grant.

ALTER TABLE catalog_items
    ADD COLUMN IF NOT EXISTS name_translations jsonb NOT NULL DEFAULT '{}'::jsonb;
