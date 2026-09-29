-- Copyright (c) 2026 Pizza 4P's. All rights reserved.
-- Proprietary and confidential. Internal use only. See LICENSE.

-- A tenant's connections to its vendors
-- ([ADR-0153](../../../../docs/adr/0153-a-vendor-is-a-provider-the-cloud-chooses.md) decisions 3–4).
--
-- A connection is a provider id from the cloud's compiled-in catalogue, the config-tree layer it
-- serves (tenant, brand or store), the provider's settings, and its secret fields. Switching a
-- tenant's e-invoice provider or courier is editing one of these rows, not a release.
--
--   * `entity_id` — the connection's id, a server-minted ULID string, so id order is creation order.
--   * `doc`       — the whole record as `jsonb`, the same store-the-shape-as-a-document choice
--                   `reason_codes` (0060) and `inventory_items` (0037) make. `pos-cloud` does the
--                   (de)serialisation; no cloud-domain type leaks into the adapter.
--
-- SECRETS ARE SEALED BEFORE THEY REACH THIS TABLE
--
-- A secret field's value is in `doc` only as XChaCha20-Poly1305 ciphertext, sealed by `pos-cloud`
-- under `integration_secret` from `cloud.toml` and bound to its tenant, connection and field. That
-- key is not in this database, so the `pg_dump` `deploy/backup.sh` ships off-box carries the
-- ciphertext without the key that opens it. No column here ever holds a credential in the clear, and
-- no read route returns the sealed values either.
--
-- The rows are T2 (Confidential) configuration: vendor endpoints, merchant codes, and sealed
-- credentials. No customer or employee identifier.
--
-- Nothing historic cites a connection by id, so DELETE is the ordinary way to remove one, and it
-- takes the sealed secrets with it. Nothing references another table, so nothing cascades.
--
-- Tenant-scoped like the rest of the config data (0012/0028/0032/0037/0060): RLS on `app.tenant_id`,
-- CRUD granted to `app_tenant`, the trusted pool owner bypassing RLS. Forward-only and additive,
-- applied idempotently on every boot (ADR-0017). Greenfield: no backfill.

CREATE TABLE IF NOT EXISTS integration_connections (
    tenant_id  text        NOT NULL,
    entity_id  text        NOT NULL,
    doc        jsonb       NOT NULL,
    updated_at timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (tenant_id, entity_id)
);

ALTER TABLE integration_connections ENABLE ROW LEVEL SECURITY;
DROP POLICY IF EXISTS integration_connections_tenant_isolation ON integration_connections;
CREATE POLICY integration_connections_tenant_isolation ON integration_connections
    USING (tenant_id = current_setting('app.tenant_id', true));
GRANT SELECT, INSERT, UPDATE, DELETE ON integration_connections TO app_tenant;
