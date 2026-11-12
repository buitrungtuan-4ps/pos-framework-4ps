-- Copyright (c) 2026 Pizza 4P's. All rights reserved.
-- Proprietary and confidential. Internal use only. See LICENSE.
--
-- 0099 — the gateway's idempotency record
-- ([ADR-0198](../../../../docs/adr/0198-a-store-reaches-the-internet-only-through-the-cloud.md)
-- decision 6): a store's idempotency key repeated within 24 hours gets the first answer back.
--
--   * `idempotency_key`     — the store's key for one act, unique per tenant and store.
--   * `gateway_endpoint_id` — the endpoint the key was first used for.
--   * `answer`              — the answer the target gave, as `jsonb`: the endpoint's declared
--                             response fields, never a request value, a header or a raw body.
--   * `kept_at_ms`          — when it was kept, from the cloud's clock, in Unix milliseconds.
--
-- RETENTION: 24 hours. A recall ignores an older row, and keeping a call deletes the store's rows
-- older than that, so nothing outlives a day by more than the store's next call. No customer or
-- employee identifier is declared on a gateway endpoint (ADR-0198 decision 9 is not built).
--
-- Tenant-scoped under RLS like the rest. Forward-only and additive, applied idempotently on every
-- boot (ADR-0017).

CREATE TABLE IF NOT EXISTS gateway_calls (
    tenant_id           text   NOT NULL,
    store_id            text   NOT NULL,
    idempotency_key     text   NOT NULL,
    gateway_endpoint_id text   NOT NULL,
    answer              jsonb  NOT NULL,
    kept_at_ms          bigint NOT NULL,
    PRIMARY KEY (tenant_id, store_id, idempotency_key)
);
CREATE INDEX IF NOT EXISTS gateway_calls_by_age ON gateway_calls (tenant_id, store_id, kept_at_ms);

ALTER TABLE gateway_calls ENABLE ROW LEVEL SECURITY;
DROP POLICY IF EXISTS gateway_calls_tenant_isolation ON gateway_calls;
CREATE POLICY gateway_calls_tenant_isolation ON gateway_calls
    USING (tenant_id = current_setting('app.tenant_id', true));
GRANT SELECT, INSERT, UPDATE, DELETE ON gateway_calls TO app_tenant;
