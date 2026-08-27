-- Copyright (c) 2026 Pizza 4P's. All rights reserved.
-- Proprietary and confidential. Internal use only. See LICENSE.

-- Kitchen master data (Track M2, ADR-0072): a store's stations and the item→station routing rules.
-- Per-store like the floor (0025) — both carry a `store_id` beside the `tenant_id`. Not PII.
--   * `kitchen_stations`      — a station a fired line routes to (Oven, Bar), with an optional backup
--                               (the printer failover target) and an `is_default` catch-all flag.
--   * `station_routing_rules` — one rule mapping a fired line (a specific item, or any line on a
--                               course) to a station, in author-controlled `sort` order.
-- Tenant-scoped exactly like every other cloud table (0011/0025): RLS keyed on `app.tenant_id`, a grant
-- to `app_tenant`, and the trusted (pool-owner) connection bypassing RLS. Referential integrity (a
-- rule's `station_id`, a station's `backup_station_id`, both rows' `store_id`) is the explicit columns
-- + RLS plus the route/publish layer's checks — soft references, not cross-table foreign keys under
-- RLS. Stations are archived, never deleted (like the floor), so a published plan that names one stays
-- reconcilable; a routing rule, like an assignment (0024), is a grant that is *removed*, not archived.
-- Forward-only and additive, applied idempotently on every boot (ADR-0017).

CREATE TABLE IF NOT EXISTS kitchen_stations (
    id                text        PRIMARY KEY,
    tenant_id         text        NOT NULL,
    store_id          text        NOT NULL,
    name              text        NOT NULL,
    backup_station_id text,
    is_default        boolean     NOT NULL DEFAULT false,
    status            text        NOT NULL DEFAULT 'active',
    created_at        timestamptz NOT NULL DEFAULT now(),
    updated_at        timestamptz NOT NULL DEFAULT now(),
    CONSTRAINT kitchen_stations_status CHECK (status IN ('active', 'archived'))
);
CREATE INDEX IF NOT EXISTS kitchen_stations_by_store ON kitchen_stations (tenant_id, store_id);

ALTER TABLE kitchen_stations ENABLE ROW LEVEL SECURITY;
DROP POLICY IF EXISTS kitchen_stations_tenant_isolation ON kitchen_stations;
CREATE POLICY kitchen_stations_tenant_isolation ON kitchen_stations
    USING (tenant_id = current_setting('app.tenant_id', true));
-- SELECT/INSERT/UPDATE only — a station is archived, never removed.
GRANT SELECT, INSERT, UPDATE ON kitchen_stations TO app_tenant;

CREATE TABLE IF NOT EXISTS station_routing_rules (
    id           text        PRIMARY KEY,
    tenant_id    text        NOT NULL,
    store_id     text        NOT NULL,
    station_id   text        NOT NULL,
    menu_item_id text,
    course_id    text,
    sort         integer     NOT NULL DEFAULT 0,
    created_at   timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX IF NOT EXISTS station_routing_rules_by_store
    ON station_routing_rules (tenant_id, store_id);

ALTER TABLE station_routing_rules ENABLE ROW LEVEL SECURITY;
DROP POLICY IF EXISTS station_routing_rules_tenant_isolation ON station_routing_rules;
CREATE POLICY station_routing_rules_tenant_isolation ON station_routing_rules
    USING (tenant_id = current_setting('app.tenant_id', true));
-- SELECT/INSERT/DELETE — a routing rule is a mapping that is removed, not archived.
GRANT SELECT, INSERT, DELETE ON station_routing_rules TO app_tenant;
