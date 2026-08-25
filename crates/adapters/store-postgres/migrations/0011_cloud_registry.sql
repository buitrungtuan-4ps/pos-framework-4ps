-- Copyright (c) 2026 Pizza 4P's. All rights reserved.
-- Proprietary and confidential. Internal use only. See LICENSE.

-- The cloud org registry (P-WS-C, ADR-0065): the four named entities the cloud has always addressed
-- by opaque ULID but never recorded — Tenant, Brand, Store, Device. Each row carries a human name, a
-- status (active/archived; rows are archived, never hard-deleted, so foreign references and history
-- stay valid), timestamps, and its parentage. This is the hierarchy `(tenant_id, store_id)` always
-- implied but never wrote down; it makes the dashboard's named pickers and the create-store flow
-- possible (no more free-text ULID entry).
--
-- The registry owns identity and naming; `config_trees` (0004) keeps owning configuration, unchanged
-- and still keyed `(tenant_id, store_id)`. A `stores` row and its config-tree row share that key.
-- Forward-only and additive, applied idempotently on every boot (ADR-0017).

-- Tenants — the root. Like `super_admin` (0003), a tenant has no parent to scope by, so it is
-- administered only by the trusted (pool-owner) connection: no RLS, no `app_tenant` grant.
CREATE TABLE IF NOT EXISTS tenants (
    tenant_id  text        PRIMARY KEY,
    name       text        NOT NULL,
    status     text        NOT NULL DEFAULT 'active',
    created_at timestamptz NOT NULL DEFAULT now(),
    updated_at timestamptz NOT NULL DEFAULT now(),
    CONSTRAINT tenants_status CHECK (status IN ('active', 'archived'))
);

-- Brands — grouped under a tenant. Tenant-scoped exactly like `config_trees`: RLS keyed on
-- `app.tenant_id`, a grant to `app_tenant`, and the trusted connection bypassing RLS to administer
-- any tenant. Parentage (`tenant_id`) is application-enforced, not a hard FK — the same no-FK posture
-- every other cloud table keeps.
CREATE TABLE IF NOT EXISTS brands (
    brand_id   text        PRIMARY KEY,
    tenant_id  text        NOT NULL,
    name       text        NOT NULL,
    status     text        NOT NULL DEFAULT 'active',
    created_at timestamptz NOT NULL DEFAULT now(),
    updated_at timestamptz NOT NULL DEFAULT now(),
    CONSTRAINT brands_status CHECK (status IN ('active', 'archived'))
);
CREATE INDEX IF NOT EXISTS brands_by_tenant ON brands (tenant_id);

ALTER TABLE brands ENABLE ROW LEVEL SECURITY;
DROP POLICY IF EXISTS brands_tenant_isolation ON brands;
CREATE POLICY brands_tenant_isolation ON brands
    USING (tenant_id = current_setting('app.tenant_id', true));
GRANT SELECT, INSERT, UPDATE ON brands TO app_tenant;

-- Stores — grouped under a tenant and, optionally, a brand (`brand_id` is nullable: a store may have
-- no brand until an operator assigns one, and the backfill below creates brand-less stores).
CREATE TABLE IF NOT EXISTS stores (
    store_id   text        PRIMARY KEY,
    tenant_id  text        NOT NULL,
    brand_id   text,
    name       text        NOT NULL,
    status     text        NOT NULL DEFAULT 'active',
    created_at timestamptz NOT NULL DEFAULT now(),
    updated_at timestamptz NOT NULL DEFAULT now(),
    CONSTRAINT stores_status CHECK (status IN ('active', 'archived'))
);
CREATE INDEX IF NOT EXISTS stores_by_tenant ON stores (tenant_id);
CREATE INDEX IF NOT EXISTS stores_by_brand  ON stores (brand_id);

ALTER TABLE stores ENABLE ROW LEVEL SECURITY;
DROP POLICY IF EXISTS stores_tenant_isolation ON stores;
CREATE POLICY stores_tenant_isolation ON stores
    USING (tenant_id = current_setting('app.tenant_id', true));
GRANT SELECT, INSERT, UPDATE ON stores TO app_tenant;

-- Devices — the canonical device identity (ADR-0065). `device_proposals` (0007, the discover→approve
-- queue) and `device_credentials` (0009, the issued secret) keep keying by the same `device_id`; this
-- table is the one record that the device exists, is named, has a kind, and belongs to a store —
-- unifying an identifier that was until now an unowned string. `kind` is left open (no CHECK) so a new
-- device kind is a data change, not a migration.
CREATE TABLE IF NOT EXISTS devices (
    device_id  text        PRIMARY KEY,
    tenant_id  text        NOT NULL,
    store_id   text        NOT NULL,
    name       text        NOT NULL,
    kind       text        NOT NULL DEFAULT 'unknown',
    status     text        NOT NULL DEFAULT 'active',
    created_at timestamptz NOT NULL DEFAULT now(),
    updated_at timestamptz NOT NULL DEFAULT now(),
    CONSTRAINT devices_status CHECK (status IN ('active', 'archived'))
);
CREATE INDEX IF NOT EXISTS devices_by_store ON devices (tenant_id, store_id);

ALTER TABLE devices ENABLE ROW LEVEL SECURITY;
DROP POLICY IF EXISTS devices_tenant_isolation ON devices;
CREATE POLICY devices_tenant_isolation ON devices
    USING (tenant_id = current_setting('app.tenant_id', true));
GRANT SELECT, INSERT, UPDATE ON devices TO app_tenant;

-- Backfill (ADR-0065): every store the cloud already configures but has never named. Seed a tenant and
-- a store row from each distinct `(tenant_id, store_id)` in `config_trees`, with a placeholder name, so
-- an upgrade of an existing cell surfaces its whole fleet immediately and naming is a non-blocking
-- follow-up. `ON CONFLICT DO NOTHING` makes this idempotent (it re-runs on every boot) and means an
-- already-archived or already-renamed row is left exactly as the operator set it — never resurrected.
INSERT INTO tenants (tenant_id, name)
SELECT DISTINCT tenant_id, 'Tenant ' || left(tenant_id, 8)
FROM config_trees
ON CONFLICT (tenant_id) DO NOTHING;

INSERT INTO stores (store_id, tenant_id, name)
SELECT DISTINCT store_id, tenant_id, 'Store ' || left(store_id, 8)
FROM config_trees
ON CONFLICT (store_id) DO NOTHING;
