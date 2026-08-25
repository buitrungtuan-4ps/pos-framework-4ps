-- Copyright (c) 2026 Pizza 4P's. All rights reserved.
-- Proprietary and confidential. Internal use only. See LICENSE.

-- The presentation tier of the cloud catalog (Phase 2a, ADR-0066 entities 11 and 12): the display
-- taxonomy a screen groups by, and the per-channel layout buttons a DisplayPlan is compiled from. This
-- is DISTINCT from the item taxonomy (0014, the operational grouping) — the operator needs both, and a
-- "Summer specials" tab may show items that report under "Pizza". The compiled DisplayPlan/LayoutBook
-- rides the `layout` config node, separate from the `menu` node the MenuBook rides, so a button moving
-- reprices nothing.
--
-- Display category/sub-category ids are the pos-proto DisplayCategoryId/DisplaySubcategoryId the
-- compiled plan carries (application-enforced parentage, the no-FK posture every cloud table keeps).
-- A layout button's identity is (tenant, sales_channel, menu_item_id) — an item has at most one button
-- per channel. Tenant-scoped exactly like the rest of the catalog: RLS on `app.tenant_id`, a grant to
-- `app_tenant`, the trusted pool owner bypassing RLS. Forward-only and additive (ADR-0017). Greenfield.

CREATE TABLE IF NOT EXISTS catalog_display_categories (
    display_category_id text        PRIMARY KEY,
    tenant_id           text        NOT NULL,
    name                text        NOT NULL,
    status              text        NOT NULL DEFAULT 'active',
    created_at          timestamptz NOT NULL DEFAULT now(),
    updated_at          timestamptz NOT NULL DEFAULT now(),
    CONSTRAINT catalog_display_categories_status CHECK (status IN ('active', 'archived'))
);
CREATE INDEX IF NOT EXISTS catalog_display_categories_by_tenant
    ON catalog_display_categories (tenant_id);

ALTER TABLE catalog_display_categories ENABLE ROW LEVEL SECURITY;
DROP POLICY IF EXISTS catalog_display_categories_tenant_isolation ON catalog_display_categories;
CREATE POLICY catalog_display_categories_tenant_isolation ON catalog_display_categories
    USING (tenant_id = current_setting('app.tenant_id', true));
GRANT SELECT, INSERT, UPDATE ON catalog_display_categories TO app_tenant;

CREATE TABLE IF NOT EXISTS catalog_display_subcategories (
    display_subcategory_id text        PRIMARY KEY,
    tenant_id              text        NOT NULL,
    display_category_id    text        NOT NULL,
    name                   text        NOT NULL,
    status                 text        NOT NULL DEFAULT 'active',
    created_at             timestamptz NOT NULL DEFAULT now(),
    updated_at             timestamptz NOT NULL DEFAULT now(),
    CONSTRAINT catalog_display_subcategories_status CHECK (status IN ('active', 'archived'))
);
CREATE INDEX IF NOT EXISTS catalog_display_subcategories_by_tenant
    ON catalog_display_subcategories (tenant_id);

ALTER TABLE catalog_display_subcategories ENABLE ROW LEVEL SECURITY;
DROP POLICY IF EXISTS catalog_display_subcategories_tenant_isolation ON catalog_display_subcategories;
CREATE POLICY catalog_display_subcategories_tenant_isolation ON catalog_display_subcategories
    USING (tenant_id = current_setting('app.tenant_id', true));
GRANT SELECT, INSERT, UPDATE ON catalog_display_subcategories TO app_tenant;

-- Layout buttons — one item's button in a channel's layout. Identity is (tenant, sales_channel,
-- menu_item_id): an item shows at most once per channel, so that triple is the primary key and the
-- upsert's conflict target. `display_subcategory_id`, `grid_column`, `grid_row` are nullable (a button
-- may sit directly under a category, and a flowing layout has no grid slot). A button is removed
-- outright, not archived — a layout edit that drops an item leaves no ghost.
CREATE TABLE IF NOT EXISTS catalog_layout_buttons (
    tenant_id              text        NOT NULL,
    sales_channel          text        NOT NULL,
    menu_item_id           text        NOT NULL,
    display_category_id    text        NOT NULL,
    display_subcategory_id text,
    label                  text        NOT NULL,
    grid_column            integer,
    grid_row               integer,
    sort                   integer     NOT NULL DEFAULT 0,
    created_at             timestamptz NOT NULL DEFAULT now(),
    updated_at             timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (tenant_id, sales_channel, menu_item_id)
);
CREATE INDEX IF NOT EXISTS catalog_layout_buttons_by_tenant_channel
    ON catalog_layout_buttons (tenant_id, sales_channel);

ALTER TABLE catalog_layout_buttons ENABLE ROW LEVEL SECURITY;
DROP POLICY IF EXISTS catalog_layout_buttons_tenant_isolation ON catalog_layout_buttons;
CREATE POLICY catalog_layout_buttons_tenant_isolation ON catalog_layout_buttons
    USING (tenant_id = current_setting('app.tenant_id', true));
GRANT SELECT, INSERT, UPDATE, DELETE ON catalog_layout_buttons TO app_tenant;
