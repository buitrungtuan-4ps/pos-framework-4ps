-- Copyright (c) 2026 Pizza 4P's. All rights reserved.
-- Proprietary and confidential. Internal use only. See LICENSE.

-- Menu sections for the cloud catalog (Phase 2a, ADR-0066 entity 7): an authoring grouping within a
-- menu — `Starters`, `Mains`, `Desserts` — that organises a menu's placements for the operator and,
-- later, a printed menu. A placement names the section it sits under via a nullable `menu_section_id`
-- on `catalog_placements` (additive, ADR-0017): an existing placement stays unsectioned until edited.
--
-- Authoring only: the compiled MenuBook is a flat set of entries with no sections, so a section
-- changes only what the operator sees while authoring — never what the edge is served, the same
-- posture modifier groups hold (0016). Tenant-scoped exactly like the rest of the catalog: RLS on
-- `app.tenant_id`, a grant to `app_tenant`, the trusted pool owner bypassing RLS. Forward-only and
-- additive, applied idempotently on every boot. Greenfield.

CREATE TABLE IF NOT EXISTS catalog_menu_sections (
    menu_section_id text        PRIMARY KEY,
    tenant_id       text        NOT NULL,
    menu_id         text        NOT NULL,
    name            text        NOT NULL,
    sort            integer     NOT NULL DEFAULT 0,
    status          text        NOT NULL DEFAULT 'active',
    created_at      timestamptz NOT NULL DEFAULT now(),
    updated_at      timestamptz NOT NULL DEFAULT now(),
    CONSTRAINT catalog_menu_sections_status CHECK (status IN ('active', 'archived'))
);
CREATE INDEX IF NOT EXISTS catalog_menu_sections_by_menu
    ON catalog_menu_sections (tenant_id, menu_id);

ALTER TABLE catalog_menu_sections ENABLE ROW LEVEL SECURITY;
DROP POLICY IF EXISTS catalog_menu_sections_tenant_isolation ON catalog_menu_sections;
CREATE POLICY catalog_menu_sections_tenant_isolation ON catalog_menu_sections
    USING (tenant_id = current_setting('app.tenant_id', true));
GRANT SELECT, INSERT, UPDATE ON catalog_menu_sections TO app_tenant;

-- A placement's optional section (nullable, additive): the section it is authored under.
ALTER TABLE catalog_placements ADD COLUMN IF NOT EXISTS menu_section_id text;
