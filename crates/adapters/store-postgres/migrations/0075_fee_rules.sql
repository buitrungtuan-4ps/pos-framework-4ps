-- Copyright (c) 2026 Pizza 4P's. All rights reserved.
-- Proprietary and confidential. Internal use only. See LICENSE.

-- The fee rules a tenant writes, at tenant, brand or store scope
-- ([ADR-0159](../../../../docs/adr/0159-a-fee-is-configuration.md) decision 1).
--
-- A fee rule is one charge a bill may add: a service charge, a packaging fee per box, a delivery
-- fee. It is authored at a scope and merged by `fee_id` down the tree, so a brand's or a store's
-- rule with the same `fee_id` replaces the tenant's for its stores. `pos-cloud` resolves each
-- store's list and publishes it whole, as the `fees` node. One row is one rule at one scope:
--
--   * `scope`      — `FEE_SCOPE_TENANT`, `_BRAND` or `_STORE`, the wire token. No check
--                    constraint, as for `setting_values` (0072): `pos-cloud` validates the scope
--                    before it writes, and widening a constraint later would need a destructive
--                    statement.
--   * `scope_id`   — the tenant, brand or store the rule was written for.
--   * `fee_id`     — the rule's stable id, a ULID: the same fee across a rename, and the key the
--                    scopes merge by.
--   * `doc`        — the rule as authored, as `jsonb`: the wire `PublishedFee`. `pos-cloud` does the
--                    (de)serialisation; no cloud-domain type leaks into the adapter.
--   * `update_time`, `updated_by` — when the rule was last written, by the cloud's clock, and the
--                    console admin who wrote it, by id.
--
-- Writing a rule again replaces it, so the key is the scope, the scope id and the fee id. Nothing
-- cites a row by anything else, so DELETE is the ordinary way to remove a rule: a store's rule
-- removed hands the store back its brand's or its tenant's. Nothing references another table.
--
-- The rows are T2 (Confidential) pricing configuration: a fee's code, name, rate or amount, and
-- which items and channels it covers. No customer or employee identifier: `updated_by` is a console
-- admin's id, never a name, and the console's audit trail (0022) records each change.
--
-- Tenant-scoped like the rest of the config data: RLS on `app.tenant_id`, CRUD granted to
-- `app_tenant`, the trusted pool owner bypassing RLS. Forward-only and additive, applied
-- idempotently on every boot (ADR-0017). Greenfield: no backfill.

CREATE TABLE IF NOT EXISTS fee_rules (
    tenant_id   text        NOT NULL,
    scope       text        NOT NULL,
    scope_id    text        NOT NULL,
    fee_id      text        NOT NULL,
    doc         jsonb       NOT NULL,
    update_time timestamptz NOT NULL,
    updated_by  text        NOT NULL,
    PRIMARY KEY (tenant_id, scope, scope_id, fee_id)
);

ALTER TABLE fee_rules ENABLE ROW LEVEL SECURITY;
DROP POLICY IF EXISTS fee_rules_tenant_isolation ON fee_rules;
CREATE POLICY fee_rules_tenant_isolation ON fee_rules
    USING (tenant_id = current_setting('app.tenant_id', true));
GRANT SELECT, INSERT, UPDATE, DELETE ON fee_rules TO app_tenant;
