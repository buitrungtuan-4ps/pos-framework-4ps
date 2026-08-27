-- Copyright (c) 2026 Pizza 4P's. All rights reserved.
-- Proprietary and confidential. Internal use only. See LICENSE.

-- People & access, foundation (Track M1, ADR-0070): the console's first record of a store's staff.
-- One row per employee — the console's first T1 Restricted / PDPD-scoped data. Tenant-scoped exactly
-- like `stores`/`brands` (0011): RLS keyed on `app.tenant_id`, a grant to `app_tenant`, and the
-- trusted (pool-owner) connection bypassing RLS to administer any tenant. Rows are archived, never
-- hard-deleted (no DELETE grant), so history and any published permission set stay reconcilable and an
-- erasure request is handled through the Data Protection contact, not an ad-hoc delete (ADR-0035).
-- Forward-only and additive, applied idempotently on every boot (ADR-0017).
--
-- Data minimization (ADR-0070): the row holds only what access control needs — a minted `id` (ULID),
-- the owning `tenant_id`, a human `name`, the tenant-unique staff `code` an operator types, a status,
-- and `pin_phc`: the **Argon2id** hash of the set PIN, NULL until a PIN is set and **never the PIN
-- itself**. No contact details, no biometrics, no behavioural or location data — this is access
-- management, not employee monitoring.
CREATE TABLE IF NOT EXISTS employees (
    id         text        PRIMARY KEY,
    tenant_id  text        NOT NULL,
    code       text        NOT NULL,
    name       text        NOT NULL,
    status     text        NOT NULL DEFAULT 'active',
    pin_phc    text,
    created_at timestamptz NOT NULL DEFAULT now(),
    updated_at timestamptz NOT NULL DEFAULT now(),
    CONSTRAINT employees_status CHECK (status IN ('active', 'archived'))
);
-- One staff code per tenant: an operator identifies a person by the code they type, so it must be
-- unique within the tenant (codes are case-sensitive badge ids, so no lower() folding).
CREATE UNIQUE INDEX IF NOT EXISTS employees_code_key ON employees (tenant_id, code);
CREATE INDEX IF NOT EXISTS employees_by_tenant ON employees (tenant_id);

ALTER TABLE employees ENABLE ROW LEVEL SECURITY;
DROP POLICY IF EXISTS employees_tenant_isolation ON employees;
CREATE POLICY employees_tenant_isolation ON employees
    USING (tenant_id = current_setting('app.tenant_id', true));
-- SELECT/INSERT/UPDATE only — no DELETE: an employee is archived, never removed (see above).
GRANT SELECT, INSERT, UPDATE ON employees TO app_tenant;
