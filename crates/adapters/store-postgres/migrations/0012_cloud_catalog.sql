-- Copyright (c) 2026 Pizza 4P's. All rights reserved.
-- Proprietary and confidential. Internal use only. See LICENSE.

-- The cloud catalog authoring model (Phase 2a, ADR-0066): the rich, normalized source of truth an
-- operator edits, from which a menu is compiled per (store × channel) into the flat MenuBook a store
-- pulls. This is the authoring side only — items, menus (with an inheritance edge), and the per-channel
-- prices an item carries in a menu. It is distinct from `config_trees` (0004), which carries the
-- *compiled* output, and from the registry (0011), which is identity and naming.
--
-- Tenant-scoped exactly like `brands`/`stores`/`devices` (0011): RLS keyed on `app.tenant_id`, a grant
-- to `app_tenant`, and the trusted (pool-owner) connection bypassing RLS to administer any tenant.
-- Parentage is application-enforced, not a hard FK — the same no-FK posture every other cloud table
-- keeps. Prices are a T2 pricing model; they live only here and in the compiled config, never a log.
-- Forward-only and additive, applied idempotently on every boot (ADR-0017). Greenfield — no backfill.

-- Items — the product master. Its id is the `menu_item_id` the compiled MenuEntry names and an inbound
-- order references. `tax_class_id` is resolved to a rate per channel at reprice time on the store.
CREATE TABLE IF NOT EXISTS catalog_items (
    menu_item_id text        PRIMARY KEY,
    tenant_id    text        NOT NULL,
    name         text        NOT NULL,
    tax_class_id text        NOT NULL,
    status       text        NOT NULL DEFAULT 'active',
    created_at   timestamptz NOT NULL DEFAULT now(),
    updated_at   timestamptz NOT NULL DEFAULT now(),
    CONSTRAINT catalog_items_status CHECK (status IN ('active', 'archived'))
);
CREATE INDEX IF NOT EXISTS catalog_items_by_tenant ON catalog_items (tenant_id);

ALTER TABLE catalog_items ENABLE ROW LEVEL SECURITY;
DROP POLICY IF EXISTS catalog_items_tenant_isolation ON catalog_items;
CREATE POLICY catalog_items_tenant_isolation ON catalog_items
    USING (tenant_id = current_setting('app.tenant_id', true));
GRANT SELECT, INSERT, UPDATE ON catalog_items TO app_tenant;

-- Menus — a named set that may inherit from a parent (`parent_menu_id` nullable: null is a root menu).
-- The compiler walks the chain and resolves most-specific-wins; a cycle is caught in the compiler, so
-- this table only stores the edge.
CREATE TABLE IF NOT EXISTS catalog_menus (
    menu_id        text        PRIMARY KEY,
    tenant_id      text        NOT NULL,
    name           text        NOT NULL,
    parent_menu_id text,
    status         text        NOT NULL DEFAULT 'active',
    created_at     timestamptz NOT NULL DEFAULT now(),
    updated_at     timestamptz NOT NULL DEFAULT now(),
    CONSTRAINT catalog_menus_status CHECK (status IN ('active', 'archived'))
);
CREATE INDEX IF NOT EXISTS catalog_menus_by_tenant ON catalog_menus (tenant_id);

ALTER TABLE catalog_menus ENABLE ROW LEVEL SECURITY;
DROP POLICY IF EXISTS catalog_menus_tenant_isolation ON catalog_menus;
CREATE POLICY catalog_menus_tenant_isolation ON catalog_menus
    USING (tenant_id = current_setting('app.tenant_id', true));
GRANT SELECT, INSERT, UPDATE ON catalog_menus TO app_tenant;

-- Placements — an item in a menu, with its per-channel prices and a published-availability floor. The
-- identity is `(menu_id, menu_item_id)`: an item appears in a menu at most once, so that pair is the
-- primary key and the upsert's conflict target. `prices` is `jsonb` (a small array of
-- `{sales_channel, unit_price}`); nothing queries into it — the compiler reads whole placements — so it
-- is stored and read as one opaque document, the same `text::jsonb` cast `config_trees` uses. A
-- placement is removed outright (not archived): a menu edit that drops an item leaves no ghost.
CREATE TABLE IF NOT EXISTS catalog_placements (
    tenant_id    text        NOT NULL,
    menu_id      text        NOT NULL,
    menu_item_id text        NOT NULL,
    prices       jsonb       NOT NULL,
    available    boolean     NOT NULL DEFAULT true,
    created_at   timestamptz NOT NULL DEFAULT now(),
    updated_at   timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (menu_id, menu_item_id)
);
CREATE INDEX IF NOT EXISTS catalog_placements_by_menu ON catalog_placements (tenant_id, menu_id);

ALTER TABLE catalog_placements ENABLE ROW LEVEL SECURITY;
DROP POLICY IF EXISTS catalog_placements_tenant_isolation ON catalog_placements;
CREATE POLICY catalog_placements_tenant_isolation ON catalog_placements
    USING (tenant_id = current_setting('app.tenant_id', true));
GRANT SELECT, INSERT, UPDATE, DELETE ON catalog_placements TO app_tenant;
