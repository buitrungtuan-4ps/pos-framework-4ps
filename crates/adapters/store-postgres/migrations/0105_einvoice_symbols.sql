-- Copyright (c) 2026 Pizza 4P's. All rights reserved.
-- Proprietary and confidential. Internal use only. See LICENSE.
--
-- 0105 — the ledger of every e-invoice symbol a tenant's stores were given
-- ([ADR-0240](../../../../docs/adr/0240-the-cloud-allocates-each-issuing-points-invoice-symbol.md)
-- decision 1), so a symbol is never given to two stores of one tenant and never reused.
--
--   * `symbol`           — the symbol, such as `1C26MAA`; unique per tenant.
--   * `store_id`         — the store it was given to, or NULL for the tenant's correction symbol.
--   * `lease_generation` — the store's lease generation when it was given, where it had one.
--   * `source`           — `MANUAL` (typed by hand) or `ALLOCATED` (by the cloud).
--   * `recorded_at_ms`   — when, from the cloud's clock, in Unix milliseconds.
--
-- RETENTION: kept for good, because a symbol once used stays used. No personal data.
--
-- Tenant-scoped under RLS like the rest. Forward-only and additive, applied idempotently on every
-- boot (ADR-0017).

CREATE TABLE IF NOT EXISTS einvoice_symbols (
    tenant_id        text   NOT NULL,
    symbol           text   NOT NULL,
    store_id         text,
    lease_generation bigint,
    source           text   NOT NULL,
    recorded_at_ms   bigint NOT NULL,
    PRIMARY KEY (tenant_id, symbol)
);

ALTER TABLE einvoice_symbols ENABLE ROW LEVEL SECURITY;
DROP POLICY IF EXISTS einvoice_symbols_tenant_isolation ON einvoice_symbols;
CREATE POLICY einvoice_symbols_tenant_isolation ON einvoice_symbols
    USING (tenant_id = current_setting('app.tenant_id', true));
GRANT SELECT, INSERT ON einvoice_symbols TO app_tenant;
