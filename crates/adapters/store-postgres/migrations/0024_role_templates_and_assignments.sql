-- Copyright (c) 2026 Pizza 4P's. All rights reserved.
-- Proprietary and confidential. Internal use only. See LICENSE.

-- People & access, roles + assignments (Track M1, ADR-0070). Two tenant-scoped tables that give a
-- store's employees (0023) their access:
--   * `role_templates`          — a tenant's named roles, each a stored subset of the pos-core
--                                 permission catalogue (§9). Not PII: names + permission-id strings.
--   * `employee_store_assignments` — the join binding a person to one of their tenant's stores with a
--                                 role. Three ids, no PII.
-- Both are tenant-scoped exactly like `employees`/`stores` (0011/0023): RLS keyed on `app.tenant_id`, a
-- grant to `app_tenant`, and the trusted (pool-owner) connection bypassing RLS to administer any
-- tenant. Referential integrity between the three ids is the explicit `tenant_id` column + RLS plus the
-- route layer's checks — the schema follows the codebase convention (e.g. `devices.store_id`, 0011) of
-- soft references rather than cross-table foreign keys under RLS.
-- Forward-only and additive, applied idempotently on every boot (ADR-0017).

-- Role templates: archived, never deleted (like employees and stores), so an assignment or a published
-- permission set that references a retired role stays reconcilable. `permissions` is a jsonb array of
-- pos-core permission ids, read/written as one opaque document via the text::jsonb cast.
CREATE TABLE IF NOT EXISTS role_templates (
    id          text        PRIMARY KEY,
    tenant_id   text        NOT NULL,
    name        text        NOT NULL,
    permissions jsonb       NOT NULL DEFAULT '[]'::jsonb,
    status      text        NOT NULL DEFAULT 'active',
    created_at  timestamptz NOT NULL DEFAULT now(),
    updated_at  timestamptz NOT NULL DEFAULT now(),
    CONSTRAINT role_templates_status CHECK (status IN ('active', 'archived'))
);
-- One role name per tenant.
CREATE UNIQUE INDEX IF NOT EXISTS role_templates_name_key ON role_templates (tenant_id, name);
CREATE INDEX IF NOT EXISTS role_templates_by_tenant ON role_templates (tenant_id);

ALTER TABLE role_templates ENABLE ROW LEVEL SECURITY;
DROP POLICY IF EXISTS role_templates_tenant_isolation ON role_templates;
CREATE POLICY role_templates_tenant_isolation ON role_templates
    USING (tenant_id = current_setting('app.tenant_id', true));
-- SELECT/INSERT/UPDATE only — no DELETE: a role is archived, never removed (see above).
GRANT SELECT, INSERT, UPDATE ON role_templates TO app_tenant;

-- Assignments: a person works at zero or more of their tenant's stores, each assignment carrying the
-- role that store grants. Removing an assignment offboards the person from that store without touching
-- the person — so, unlike employees/roles, an assignment IS removable (a DELETE grant), and the audit
-- trail (ADR-0069) records who removed it.
CREATE TABLE IF NOT EXISTS employee_store_assignments (
    id               text        PRIMARY KEY,
    tenant_id        text        NOT NULL,
    employee_id      text        NOT NULL,
    store_id         text        NOT NULL,
    role_template_id text        NOT NULL,
    created_at       timestamptz NOT NULL DEFAULT now()
);
-- A person is assigned to a given store at most once (their role there is a single template).
CREATE UNIQUE INDEX IF NOT EXISTS employee_store_assignments_unique
    ON employee_store_assignments (tenant_id, employee_id, store_id);
CREATE INDEX IF NOT EXISTS employee_store_assignments_by_store
    ON employee_store_assignments (tenant_id, store_id);
CREATE INDEX IF NOT EXISTS employee_store_assignments_by_employee
    ON employee_store_assignments (tenant_id, employee_id);

ALTER TABLE employee_store_assignments ENABLE ROW LEVEL SECURITY;
DROP POLICY IF EXISTS employee_store_assignments_tenant_isolation ON employee_store_assignments;
CREATE POLICY employee_store_assignments_tenant_isolation ON employee_store_assignments
    USING (tenant_id = current_setting('app.tenant_id', true));
-- SELECT/INSERT/DELETE — an assignment is a grant that is removed (offboarding), not archived.
GRANT SELECT, INSERT, DELETE ON employee_store_assignments TO app_tenant;
