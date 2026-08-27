-- Copyright (c) 2026 Pizza 4P's. All rights reserved.
-- Proprietary and confidential. Internal use only. See LICENSE.

-- Console audit trail ([ADR-0069](../../../../docs/adr/0069-audit-trail.md), Track G2). One append-only
-- row per console mutation: who did it (a snapshot of the acting admin, so renaming or deleting the
-- admin later never rewrites history), what they did, to which entity, and the before/after of the
-- change. This is the console-administration equivalent of the domain event log (0001) — a different
-- audience (operators, not stores) and deliberately not on the store-replicated stream.
--
--   * `id`             — a ULID minted at the edge, the row's identity and time-ordered key.
--   * `tenant_id`      — the tenant the action belongs to, or NULL for a tenant-global action
--                        (creating a tenant, admin management, the break-glass reset).
--   * `actor_*`        — the acting admin snapshotted at action time (id, email, role).
--   * `action`         — `resource.verb` (e.g. `store.update`, `apikey.revoke`).
--   * `entity_type`/`entity_id` — the affected entity.
--   * `before`/`after` — the change as jsonb; `before` is NULL for a create, `after` for a delete.
--   * `request_id`     — a correlation id, NULL until a request-id middleware lands.
--   * `at`             — Unix ms of the action.
--
-- Append-only is enforced at the grant: the query role gets SELECT and INSERT, never UPDATE or DELETE,
-- so a written row cannot be altered or removed through the application role. RLS-isolated by tenant
-- exactly like `config_trees`: a query role assuming app_tenant sees only its own tenant's rows (and
-- never the NULL-tenant global ones); the trusted pool-owner connection the server runs as bypasses
-- RLS to write any tenant's row and to read across tenants for the audit screen. Forward-only and
-- additive, applied idempotently on every boot (ADR-0017).
CREATE TABLE IF NOT EXISTS audit_log (
    id             text   NOT NULL PRIMARY KEY,
    tenant_id      text,
    actor_admin_id text   NOT NULL,
    actor_email    text   NOT NULL,
    actor_role     text   NOT NULL,
    action         text   NOT NULL,
    entity_type    text   NOT NULL,
    entity_id      text   NOT NULL,
    before         jsonb,
    after          jsonb,
    request_id     text,
    at             bigint NOT NULL
);
-- The audit screen reads a tenant's entries newest-first; this index answers that without a scan.
CREATE INDEX IF NOT EXISTS audit_log_by_tenant_at ON audit_log (tenant_id, at DESC);
-- A per-entity history (the audit tab on a Detail view) filters by entity then orders newest-first.
CREATE INDEX IF NOT EXISTS audit_log_by_entity
    ON audit_log (tenant_id, entity_type, entity_id, at DESC);

ALTER TABLE audit_log ENABLE ROW LEVEL SECURITY;
DROP POLICY IF EXISTS audit_log_tenant_isolation ON audit_log;
CREATE POLICY audit_log_tenant_isolation ON audit_log
    USING (tenant_id = current_setting('app.tenant_id', true));
-- Append-only: SELECT and INSERT only — no UPDATE or DELETE grant, so the application role can never
-- alter or remove a written audit row.
GRANT SELECT, INSERT ON audit_log TO app_tenant;
