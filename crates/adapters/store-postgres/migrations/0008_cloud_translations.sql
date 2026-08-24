-- Copyright (c) 2026 Pizza 4P's. All rights reserved.
-- Proprietary and confidential. Internal use only. See LICENSE.

-- Translation grid: a tenant's localized menu/content strings (P7, ADR-0043), feeding the edge's ICU
-- i18n runtime (ADR-0020). One jsonb document per tenant — key → { locale → string } — authored and
-- read whole, exactly like the config tree. Forward-only and additive, applied idempotently on every
-- boot (ADR-0017).
--
--  * `grid` is the whole `{ "menu.item.pho": { "en": "Pho", "vi": "Phở" }, … }` map. The cloud
--    enforces at authoring time that every key carries a non-empty `en` (the always-present
--    fallback), so this column never needs a per-key constraint.
--  * `tenant_id` is the primary key and the RLS predicate: one grid per tenant, isolated by tenant.
CREATE TABLE IF NOT EXISTS translations (
    tenant_id  text        PRIMARY KEY,
    grid       jsonb       NOT NULL,
    updated_at timestamptz NOT NULL DEFAULT now()
);

ALTER TABLE translations ENABLE ROW LEVEL SECURITY;
DROP POLICY IF EXISTS translations_tenant_isolation ON translations;
CREATE POLICY translations_tenant_isolation ON translations
    USING (tenant_id = current_setting('app.tenant_id', true));
GRANT SELECT, INSERT, UPDATE, DELETE ON translations TO app_tenant;
