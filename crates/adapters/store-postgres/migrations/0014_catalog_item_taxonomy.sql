-- Copyright (c) 2026 Pizza 4P's. All rights reserved.
-- Proprietary and confidential. Internal use only. See LICENSE.

-- The operational item taxonomy for the cloud catalog (Phase 2a, ADR-0066 entities 2 and 3): item
-- categories and their sub-categories, the grouping a product-mix report totals by and a kitchen
-- station groups on. This is DISTINCT from the presentation taxonomy a layout groups by (entity 11,
-- delivered in the layout node) — the operator needs both, and they do not coincide.
--
-- Two new tables plus two additive columns on `catalog_items` linking an item to its category and
-- sub-category (both nullable: an unclassified item is valid). Parentage is application-enforced, not
-- a hard FK — the no-FK posture every cloud table keeps. Tenant-scoped exactly like the rest of the
-- catalog (0012/0013): RLS on `app.tenant_id`, a grant to `app_tenant`, the trusted pool owner
-- bypassing RLS. Forward-only and additive, applied idempotently on every boot (ADR-0017). Greenfield.

CREATE TABLE IF NOT EXISTS catalog_item_categories (
    item_category_id text        PRIMARY KEY,
    tenant_id        text        NOT NULL,
    name             text        NOT NULL,
    status           text        NOT NULL DEFAULT 'active',
    created_at       timestamptz NOT NULL DEFAULT now(),
    updated_at       timestamptz NOT NULL DEFAULT now(),
    CONSTRAINT catalog_item_categories_status CHECK (status IN ('active', 'archived'))
);
CREATE INDEX IF NOT EXISTS catalog_item_categories_by_tenant ON catalog_item_categories (tenant_id);

ALTER TABLE catalog_item_categories ENABLE ROW LEVEL SECURITY;
DROP POLICY IF EXISTS catalog_item_categories_tenant_isolation ON catalog_item_categories;
CREATE POLICY catalog_item_categories_tenant_isolation ON catalog_item_categories
    USING (tenant_id = current_setting('app.tenant_id', true));
GRANT SELECT, INSERT, UPDATE ON catalog_item_categories TO app_tenant;

-- Sub-categories carry their parent category id (application-enforced).
CREATE TABLE IF NOT EXISTS catalog_item_subcategories (
    item_subcategory_id text        PRIMARY KEY,
    tenant_id           text        NOT NULL,
    item_category_id    text        NOT NULL,
    name                text        NOT NULL,
    status              text        NOT NULL DEFAULT 'active',
    created_at          timestamptz NOT NULL DEFAULT now(),
    updated_at          timestamptz NOT NULL DEFAULT now(),
    CONSTRAINT catalog_item_subcategories_status CHECK (status IN ('active', 'archived'))
);
CREATE INDEX IF NOT EXISTS catalog_item_subcategories_by_tenant
    ON catalog_item_subcategories (tenant_id);

ALTER TABLE catalog_item_subcategories ENABLE ROW LEVEL SECURITY;
DROP POLICY IF EXISTS catalog_item_subcategories_tenant_isolation ON catalog_item_subcategories;
CREATE POLICY catalog_item_subcategories_tenant_isolation ON catalog_item_subcategories
    USING (tenant_id = current_setting('app.tenant_id', true));
GRANT SELECT, INSERT, UPDATE ON catalog_item_subcategories TO app_tenant;

-- Link an item to its operational category and sub-category. Both nullable: unclassified is valid,
-- and the columns default to NULL so existing rows are untouched.
ALTER TABLE catalog_items ADD COLUMN IF NOT EXISTS item_category_id text;
ALTER TABLE catalog_items ADD COLUMN IF NOT EXISTS item_subcategory_id text;
