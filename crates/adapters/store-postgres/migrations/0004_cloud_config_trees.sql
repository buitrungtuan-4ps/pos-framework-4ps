-- Copyright (c) 2026 Pizza 4P's. All rights reserved.
-- Proprietary and confidential. Internal use only. See LICENSE.

-- The four-level configuration tree the cloud owns and publishes (P7, ADR-0033). One row per
-- (tenant, store): `state` is the whole ConfigTreeState — the four authored layers (Tenant → Brand →
-- Store → Device) and the published version history — as jsonb, so loading a store's tree is a single
-- primary-key lookup and saving it replaces the row. Derived and mutable, so jsonb (unlike the event
-- log's byte-preserving json). Forward-only and additive, applied idempotently on every boot
-- (ADR-0017).
--
-- Keyed and RLS-isolated by tenant, exactly as the rollups read model is: a store's config is its
-- tenant's data, and the admin path scopes every read and write to a tenant, so the key confines a
-- tree to its owning tenant and the policy is the belt-and-suspenders behind it. The trusted admin
-- connection (the pool owner) bypasses RLS to author any tenant's tree; a query role assuming
-- app_tenant is filtered to its own.
CREATE TABLE IF NOT EXISTS config_trees (
    tenant_id  text        NOT NULL,
    store_id   text        NOT NULL,
    state      jsonb       NOT NULL,
    updated_at timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (tenant_id, store_id)
);

ALTER TABLE config_trees ENABLE ROW LEVEL SECURITY;
DROP POLICY IF EXISTS config_trees_tenant_isolation ON config_trees;
CREATE POLICY config_trees_tenant_isolation ON config_trees
    USING (tenant_id = current_setting('app.tenant_id', true));
GRANT SELECT, INSERT, UPDATE ON config_trees TO app_tenant;
