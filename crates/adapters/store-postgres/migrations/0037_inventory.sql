-- Copyright (c) 2026 Pizza 4P's. All rights reserved.
-- Proprietary and confidential. Internal use only. See LICENSE.

-- Inventory authoring: a tenant's ingredients, per-item/modifier recipes, and supplier references
-- ([ADR-0079](../../../../docs/adr/0079-inventory-and-suppliers.md), Track M6). Where an operator's
-- inventory master lives between edits, before a publish assembles it into the `inventory` config node
-- the edge's §8 engine reads. Until M6 the inventory domain (recipes, stock projection, auto-86) was
-- finished but had no inputs: nothing could author a recipe.
--
-- The three entity kinds (ingredient, recipe, supplier) share one shape — a tenant, a stable id, and a
-- document — so they live in one table discriminated by `kind` rather than three near-identical tables:
--
--   * `kind`      — 'ingredient' | 'recipe' | 'supplier'. A recipe's id is the menu item / modifier it
--                   makes; an ingredient's and a supplier's id is its own.
--   * `entity_id` — the record's id within its (tenant, kind) — a ULID string, so ordering by it is
--                   creation order for a stable diff.
--   * `doc`       — the whole authored record (the wire `PublishedIngredient` / `PublishedRecipe` /
--                   `PublishedSupplier`) held as `jsonb`, the same store-the-shape-as-a-document choice
--                   `campaigns` (0032) and `config_trees` make. `pos-cloud` does the (de)serialisation;
--                   no cloud-domain type leaks into the adapter.
--
-- CRUD is per-record (a tenant edits one ingredient or one recipe at a time), so `app_tenant` holds
-- INSERT/UPDATE/DELETE as well as SELECT. Recipe quantities and supplier terms are T2 (Confidential)
-- configuration; the row carries no customer identifier. Tenant-scoped exactly like the rest of the
-- config data (0012/0028/0032): RLS on `app.tenant_id`, a grant to `app_tenant`, the trusted pool owner
-- bypassing RLS. Forward-only and additive, applied idempotently on every boot (ADR-0017). Greenfield —
-- no backfill.

CREATE TABLE IF NOT EXISTS inventory_items (
    tenant_id  text        NOT NULL,
    kind       text        NOT NULL,
    entity_id  text        NOT NULL,
    doc        jsonb       NOT NULL,
    updated_at timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (tenant_id, kind, entity_id)
);
CREATE INDEX IF NOT EXISTS inventory_items_by_kind ON inventory_items (tenant_id, kind);

ALTER TABLE inventory_items ENABLE ROW LEVEL SECURITY;
DROP POLICY IF EXISTS inventory_items_tenant_isolation ON inventory_items;
CREATE POLICY inventory_items_tenant_isolation ON inventory_items
    USING (tenant_id = current_setting('app.tenant_id', true));
GRANT SELECT, INSERT, UPDATE, DELETE ON inventory_items TO app_tenant;
