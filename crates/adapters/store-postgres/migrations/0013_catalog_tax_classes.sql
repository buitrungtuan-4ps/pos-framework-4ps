-- Copyright (c) 2026 Pizza 4P's. All rights reserved.
-- Proprietary and confidential. Internal use only. See LICENSE.

-- Tax classes for the cloud catalog (Phase 2a, ADR-0066 entity 10). A named bucket an item belongs to
-- (`Standard 10%`, `Alcohol`, `Takeaway reduced`); the *rate* for each (tax_class, channel) lives in
-- the store's locale pack, not here. This table exists so an operator picks a tax class **by name**
-- when authoring an item instead of pasting a ULID — the same "kill the ULID" move the registry (0011)
-- made for tenants and stores. Its `tax_class_id` is the id an item's `catalog_items.tax_class_id`
-- references (application-enforced, not a hard FK — the no-FK posture every cloud table keeps).
--
-- Tenant-scoped exactly like the rest of the catalog (0012): RLS on `app.tenant_id`, a grant to
-- `app_tenant`, the trusted pool owner bypassing RLS. Forward-only and additive, applied idempotently
-- on every boot (ADR-0017). Greenfield — no backfill.

CREATE TABLE IF NOT EXISTS catalog_tax_classes (
    tax_class_id text        PRIMARY KEY,
    tenant_id    text        NOT NULL,
    name         text        NOT NULL,
    status       text        NOT NULL DEFAULT 'active',
    created_at   timestamptz NOT NULL DEFAULT now(),
    updated_at   timestamptz NOT NULL DEFAULT now(),
    CONSTRAINT catalog_tax_classes_status CHECK (status IN ('active', 'archived'))
);
CREATE INDEX IF NOT EXISTS catalog_tax_classes_by_tenant ON catalog_tax_classes (tenant_id);

ALTER TABLE catalog_tax_classes ENABLE ROW LEVEL SECURITY;
DROP POLICY IF EXISTS catalog_tax_classes_tenant_isolation ON catalog_tax_classes;
CREATE POLICY catalog_tax_classes_tenant_isolation ON catalog_tax_classes
    USING (tenant_id = current_setting('app.tenant_id', true));
GRANT SELECT, INSERT, UPDATE ON catalog_tax_classes TO app_tenant;
