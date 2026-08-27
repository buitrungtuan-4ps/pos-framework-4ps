-- Copyright (c) 2026 Pizza 4P's. All rights reserved.
-- Proprietary and confidential. Internal use only. See LICENSE.

-- Floor master data (Track M2, ADR-0072): a store's areas and the tables in each. Unlike the catalog
-- (per-tenant) these are inherently per-store — a floor is a physical room — so both tables carry a
-- `store_id` beside the `tenant_id`. Neither is PII: an area is a name, a table is a label, a seat
-- count and an optional grid position.
--   * `floor_areas`  — a named region of the floor (a terrace, the main hall).
--   * `floor_tables` — a table, belonging to an area, that the floor editor places on a grid.
-- Tenant-scoped exactly like every other cloud table (0011/0023): RLS keyed on `app.tenant_id`, a
-- grant to `app_tenant`, and the trusted (pool-owner) connection bypassing RLS to administer any
-- tenant. Referential integrity (a table's `area_id`, and both rows' `store_id`) is the explicit
-- columns + RLS plus the route layer's checks — soft references, not cross-table foreign keys under
-- RLS, the codebase convention (e.g. `devices.store_id`, 0011). Both are archived, never deleted (like
-- employees/stores), so a published floor plan and any order history that names a table stay
-- reconcilable. Forward-only and additive, applied idempotently on every boot (ADR-0017).

CREATE TABLE IF NOT EXISTS floor_areas (
    id         text        PRIMARY KEY,
    tenant_id  text        NOT NULL,
    store_id   text        NOT NULL,
    name       text        NOT NULL,
    status     text        NOT NULL DEFAULT 'active',
    created_at timestamptz NOT NULL DEFAULT now(),
    updated_at timestamptz NOT NULL DEFAULT now(),
    CONSTRAINT floor_areas_status CHECK (status IN ('active', 'archived'))
);
CREATE INDEX IF NOT EXISTS floor_areas_by_store ON floor_areas (tenant_id, store_id);

ALTER TABLE floor_areas ENABLE ROW LEVEL SECURITY;
DROP POLICY IF EXISTS floor_areas_tenant_isolation ON floor_areas;
CREATE POLICY floor_areas_tenant_isolation ON floor_areas
    USING (tenant_id = current_setting('app.tenant_id', true));
-- SELECT/INSERT/UPDATE only — an area is archived, never removed.
GRANT SELECT, INSERT, UPDATE ON floor_areas TO app_tenant;

CREATE TABLE IF NOT EXISTS floor_tables (
    id          text        PRIMARY KEY,
    tenant_id   text        NOT NULL,
    store_id    text        NOT NULL,
    area_id     text        NOT NULL,
    label       text        NOT NULL,
    seats       integer     NOT NULL DEFAULT 0,
    grid_column integer,
    grid_row    integer,
    status      text        NOT NULL DEFAULT 'active',
    created_at  timestamptz NOT NULL DEFAULT now(),
    updated_at  timestamptz NOT NULL DEFAULT now(),
    CONSTRAINT floor_tables_status CHECK (status IN ('active', 'archived')),
    CONSTRAINT floor_tables_seats CHECK (seats >= 0)
);
CREATE INDEX IF NOT EXISTS floor_tables_by_store ON floor_tables (tenant_id, store_id);
CREATE INDEX IF NOT EXISTS floor_tables_by_area ON floor_tables (tenant_id, area_id);

ALTER TABLE floor_tables ENABLE ROW LEVEL SECURITY;
DROP POLICY IF EXISTS floor_tables_tenant_isolation ON floor_tables;
CREATE POLICY floor_tables_tenant_isolation ON floor_tables
    USING (tenant_id = current_setting('app.tenant_id', true));
-- SELECT/INSERT/UPDATE only — a table is archived, never removed.
GRANT SELECT, INSERT, UPDATE ON floor_tables TO app_tenant;
