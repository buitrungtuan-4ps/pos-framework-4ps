-- Copyright (c) 2026 Pizza 4P's. All rights reserved.
-- Proprietary and confidential. Internal use only. See LICENSE.

-- An assignment reaches one store, a store group, or every store of the tenant
-- ([ADR-0158](../../../../docs/adr/0158-the-till-enforces-each-persons-own-permissions.md)
-- decision 3).
--
-- `employee_store_assignments` (0024) binds a person to one store with a role, and stays exactly as
-- it is: every row in it keeps meaning what it meant, and a rollback to a release without this
-- table loses only the wider grants. This table holds the two wider scopes:
--
--   * `STORE_GROUP` — the person holds the role at every store the group holds
--     ([ADR-0122](../../../../docs/adr/0122-a-store-group-is-a-delivery-cohort.md)), as its
--     membership changes. `store_group_id` names the group.
--   * `TENANT`      — the person holds the role at every store of the tenant, one opened later
--     included. `store_group_id` is NULL.
--
-- A person holds at most one assignment per group and one tenant-wide, as they hold at most one per
-- store: two partial unique indexes, one for each scope. A person may hold a store assignment and a
-- wider one at the same store; the compiler gives them the union of the roles and the highest
-- ceiling, as it does for any two.
--
-- No foreign keys, as the sibling table has none: the route checks every id, and RLS partitions the
-- tables a key would cross. An assignment names three ids and a scope, no personal data, and is
-- removed rather than archived, so the grant is SELECT, INSERT and DELETE, as on the sibling.
--
-- Forward-only and additive, applied idempotently on every boot (ADR-0017).

CREATE TABLE IF NOT EXISTS employee_scope_assignments (
    assignment_id    text        PRIMARY KEY,
    tenant_id        text        NOT NULL,
    employee_id      text        NOT NULL,
    role_template_id text        NOT NULL,
    scope_kind       text        NOT NULL,
    store_group_id   text,
    create_time      timestamptz NOT NULL DEFAULT now(),
    CONSTRAINT employee_scope_assignments_scope_kind
        CHECK (scope_kind IN ('STORE_GROUP', 'TENANT')),
    CONSTRAINT employee_scope_assignments_group_iff_store_group
        CHECK ((scope_kind = 'STORE_GROUP') = (store_group_id IS NOT NULL))
);

CREATE UNIQUE INDEX IF NOT EXISTS employee_scope_assignments_one_per_group
    ON employee_scope_assignments (tenant_id, employee_id, store_group_id)
    WHERE scope_kind = 'STORE_GROUP';
CREATE UNIQUE INDEX IF NOT EXISTS employee_scope_assignments_one_tenant_wide
    ON employee_scope_assignments (tenant_id, employee_id)
    WHERE scope_kind = 'TENANT';
-- "Who works at this store" reads a tenant's tenant-wide rows and the rows naming the store's
-- groups; "who holds a role" and "where does this person work" read by role and by person.
CREATE INDEX IF NOT EXISTS employee_scope_assignments_by_scope
    ON employee_scope_assignments (tenant_id, scope_kind, store_group_id);
CREATE INDEX IF NOT EXISTS employee_scope_assignments_by_employee
    ON employee_scope_assignments (tenant_id, employee_id);

ALTER TABLE employee_scope_assignments ENABLE ROW LEVEL SECURITY;
DROP POLICY IF EXISTS employee_scope_assignments_tenant_isolation ON employee_scope_assignments;
CREATE POLICY employee_scope_assignments_tenant_isolation ON employee_scope_assignments
    USING (tenant_id = current_setting('app.tenant_id', true));
GRANT SELECT, INSERT, DELETE ON employee_scope_assignments TO app_tenant;
