-- Copyright (c) 2026 Pizza 4P's. All rights reserved.
-- Proprietary and confidential. Internal use only. See LICENSE.
--
-- 0098 — a tenant's gateway endpoints
-- ([ADR-0198](../../../../docs/adr/0198-a-store-reaches-the-internet-only-through-the-cloud.md)
-- decisions 2 and 3): the `https` targets a store's lookups name by id, through the cloud.
--
--   * `entity_id` — the endpoint's id, a server-minted ULID string, so id order is creation order.
--   * `doc`       — the whole record as `jsonb`, the shape `integration_connections` (0070) stores;
--                   `pos-cloud` does the (de)serialisation.
--
-- SECRET HEADERS ARE SEALED BEFORE THEY REACH THIS TABLE, under `integration_secret` from
-- `cloud.toml`, bound to the tenant, the endpoint and the header, exactly as 0070's connection
-- secrets. The key is not in this database, so a `pg_dump` carries no credential in the clear.
--
-- The rows are T2 (Confidential) configuration. No customer or employee identifier.
--
-- Tenant-scoped like the rest of the config data: RLS on `app.tenant_id`, CRUD granted to
-- `app_tenant`, the trusted pool owner bypassing RLS. Forward-only and additive, applied
-- idempotently on every boot (ADR-0017).

CREATE TABLE IF NOT EXISTS gateway_endpoints (
    tenant_id  text        NOT NULL,
    entity_id  text        NOT NULL,
    doc        jsonb       NOT NULL,
    updated_at timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (tenant_id, entity_id)
);

ALTER TABLE gateway_endpoints ENABLE ROW LEVEL SECURITY;
DROP POLICY IF EXISTS gateway_endpoints_tenant_isolation ON gateway_endpoints;
CREATE POLICY gateway_endpoints_tenant_isolation ON gateway_endpoints
    USING (tenant_id = current_setting('app.tenant_id', true));
GRANT SELECT, INSERT, UPDATE, DELETE ON gateway_endpoints TO app_tenant;
