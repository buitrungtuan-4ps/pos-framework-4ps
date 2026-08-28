-- Copyright (c) 2026 Pizza 4P's. All rights reserved.
-- Proprietary and confidential. Internal use only. See LICENSE.

-- The per-(tax class × sales channel) tax rate table (Track M4, ADR-0074). The rate the tax *class*
-- (0013) resolves to, keyed by channel — Japan's worked example is the same item at 8% takeaway and
-- 10% dine-in (docs/pos-spec.md §5). Until M4 this table had no home: `pos_proto::TaxRateTable` and
-- the edge billing that reads it were built (ADR-0028), but a rate was only ever the hardcoded
-- bootstrap default. This is where an operator authors it; a publish assembles these rows into the
-- `tax` config node the edge applies.
--
-- The rate is stored in **basis points** (`rate_bps`, matching `pos_proto::TaxRate`): integer, because
-- `clippy.toml` bans floating point and a rate on a legal document is not `0.09999`. 10% is 1000, the
-- reduced 8% is 800; bounded to [0, 10000] so a typo cannot bill 500% tax.
--
-- Tenant-scoped exactly like the rest of the catalog (0012/0013): RLS on `app.tenant_id`, a grant to
-- `app_tenant`, the trusted pool owner bypassing RLS. `tax_class_id` references a `catalog_tax_classes`
-- row (application-enforced, the no-FK posture every cloud table keeps). The whole tenant table is
-- replaced on each save, so `app_tenant` also holds DELETE here (config data, not an append-only log).
-- Forward-only and additive, applied idempotently on every boot (ADR-0017). Greenfield — no backfill.

CREATE TABLE IF NOT EXISTS catalog_tax_rates (
    tenant_id     text        NOT NULL,
    tax_class_id  text        NOT NULL,
    sales_channel text        NOT NULL,
    rate_bps      integer     NOT NULL,
    updated_at    timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (tenant_id, tax_class_id, sales_channel),
    CONSTRAINT catalog_tax_rates_bps CHECK (rate_bps >= 0 AND rate_bps <= 10000)
);
CREATE INDEX IF NOT EXISTS catalog_tax_rates_by_tenant ON catalog_tax_rates (tenant_id);

ALTER TABLE catalog_tax_rates ENABLE ROW LEVEL SECURITY;
DROP POLICY IF EXISTS catalog_tax_rates_tenant_isolation ON catalog_tax_rates;
CREATE POLICY catalog_tax_rates_tenant_isolation ON catalog_tax_rates
    USING (tenant_id = current_setting('app.tenant_id', true));
GRANT SELECT, INSERT, UPDATE, DELETE ON catalog_tax_rates TO app_tenant;
