-- Copyright (c) 2026 Pizza 4P's. All rights reserved.
-- Proprietary and confidential. Internal use only. See LICENSE.

-- The values a tenant writes for its settings, at tenant, brand, store-group or store scope
-- ([ADR-0160](../../../../docs/adr/0160-everything-a-store-runs-differently-is-published-configuration.md)
-- decision 3).
--
-- A setting is a field of a published node, listed in the register `pos-proto` generates
-- (`docs/configuration.md`). One row is one value at one scope:
--
--   * `setting_key` — the setting's `node.field`, such as `shift.no_shift_selling`.
--   * `scope`       — `SETTING_SCOPE_TENANT`, `_BRAND`, `_STORE_GROUP` or `_STORE`, the wire token.
--                     No check constraint, unlike other enum columns (naming-and-api.md §6): the
--                     register adds a device scope with per-device entries (ADR-0160 decision 4),
--                     and widening a constraint would need a destructive statement. `pos-cloud`
--                     validates every value against the register before it writes one.
--   * `scope_id`    — the tenant, brand, store group or store the value was written for.
--   * `doc`         — the whole value as `jsonb`, the same store-the-shape-as-a-document choice
--                     `integration_connections` (0070) makes. `pos-cloud` does the
--                     (de)serialisation; no cloud-domain type leaks into the adapter.
--
-- Writing a value again replaces it, so the key is the setting, the scope and the scope id. Nothing
-- cites a value by id, so DELETE is the ordinary way to clear one. Nothing references another table.
--
-- The rows are T3 (Internal) configuration: which way a store runs. No customer or employee
-- identifier, and who changed what is in the console's audit trail (0022), not here.
--
-- Tenant-scoped like the rest of the config data: RLS on `app.tenant_id`, CRUD granted to
-- `app_tenant`, the trusted pool owner bypassing RLS. Forward-only and additive, applied
-- idempotently on every boot (ADR-0017). Greenfield: no backfill.

CREATE TABLE IF NOT EXISTS setting_values (
    tenant_id   text        NOT NULL,
    setting_key text        NOT NULL,
    scope       text        NOT NULL,
    scope_id    text        NOT NULL,
    doc         jsonb       NOT NULL,
    update_time timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (tenant_id, setting_key, scope, scope_id)
);

ALTER TABLE setting_values ENABLE ROW LEVEL SECURITY;
DROP POLICY IF EXISTS setting_values_tenant_isolation ON setting_values;
CREATE POLICY setting_values_tenant_isolation ON setting_values
    USING (tenant_id = current_setting('app.tenant_id', true));
GRANT SELECT, INSERT, UPDATE, DELETE ON setting_values TO app_tenant;
