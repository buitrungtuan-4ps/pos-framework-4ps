-- Copyright (c) 2026 Pizza 4P's. All rights reserved.
-- Proprietary and confidential. Internal use only. See LICENSE.

-- A version row per tenant for the tax-rate table (Q3c slice 5b, ADR-0095 shape C). Forward-only and
-- additive, applied idempotently on every boot (ADR-0017). Greenfield — no backfill: a tenant with no
-- row yet has never saved rates, which is exactly the state a first save asserts.
--
-- # Why this table exists at all, when nothing else in ADR-0094's work needed one
--
-- Every other conditional write in the console compares `xmin`, the row's own system column, which
-- costs no schema change. That works because those writes *update a row in place*. `catalog_tax_rates`
-- (0028) is many rows per tenant and a save **replaces** them — `DELETE` then `INSERT`, in one
-- transaction. Every row's `xmin` is destroyed and a new one minted, so there is no row whose version
-- survives a save to compare against. The collection is the entity, and the entity needs somewhere to
-- keep its version.
--
-- The translation grid, which ADR-0095 booked alongside this one, turned out **not** to need this: it
-- is a single jsonb row per tenant (0008), so its own `xmin` works and it stays migration-free.
--
-- # The version is this row's `xmin`, not a counter
--
-- There is no `version` column here on purpose. The row exists so that *something* holds an `xmin`
-- across a replace; touching it inside the same transaction as the DELETE/INSERT moves that `xmin`,
-- and the token stays the same opaque shape as every other version in this work (ADR-0094: the seam
-- never reads into a token). A counter would be a second, differently-shaped version concept for one
-- table, and a wrap-around or a reset to reason about for no gain.
--
-- Tenant-scoped like the table it versions: RLS on `app.tenant_id`, a grant to `app_tenant`, the
-- trusted pool owner bypassing RLS.

CREATE TABLE IF NOT EXISTS catalog_tax_rate_versions (
    tenant_id  text        PRIMARY KEY,
    updated_at timestamptz NOT NULL DEFAULT now()
);

ALTER TABLE catalog_tax_rate_versions ENABLE ROW LEVEL SECURITY;
DROP POLICY IF EXISTS catalog_tax_rate_versions_tenant_isolation ON catalog_tax_rate_versions;
CREATE POLICY catalog_tax_rate_versions_tenant_isolation ON catalog_tax_rate_versions
    USING (tenant_id = current_setting('app.tenant_id', true));
GRANT SELECT, INSERT, UPDATE, DELETE ON catalog_tax_rate_versions TO app_tenant;
