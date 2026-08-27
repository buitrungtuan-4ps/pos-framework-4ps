-- Copyright (c) 2026 Pizza 4P's. All rights reserved.
-- Proprietary and confidential. Internal use only. See LICENSE.

-- Operational alerts ([ADR-0073](../../../../docs/adr/0073-alerting.md), Track O2). One row per alert
-- with an open→resolved lifecycle: the evaluator opens (or refreshes) an alert while a condition is
-- firing and resolves it when the condition clears, so the list reflects reality rather than accreting
-- stale rows. This is the operational counterpart to the console audit trail (0022) — a different
-- audience (whoever watches the fleet) and a mutable lifecycle rather than append-only.
--
--   * `id`              — a ULID minted when the alert is first opened, the row's identity.
--   * `tenant_id`       — the tenant the alert belongs to, or NULL for a server-wide condition
--                         (projector health, JetStream capacity).
--   * `kind`            — the condition (`AlertKind::as_str`: `store_offline`, `relay_backlog`, …).
--   * `dedup_key`       — the scope within the kind: a store id, an endpoint id, or '' for a
--                         server-wide singleton. Together with (tenant_id, kind) it identifies the one
--                         open alert for a condition.
--   * `severity`        — `AlertSeverity::as_str` (`info` | `warning` | `critical`).
--   * `summary`         — a one-line human summary composed when opened/refreshed.
--   * `detail`          — the numbers behind the alert as jsonb (counts, ages, a version); never a
--                         payload or PII.
--   * `first_seen_at`   — Unix ms the alert was opened.
--   * `last_seen_at`    — Unix ms the condition was last observed still firing (refreshed each tick).
--   * `resolved_at`     — Unix ms the condition cleared, or NULL while the alert is active.
--   * `acknowledged_at` — Unix ms an operator acknowledged it, or NULL.
--
-- RLS-isolated by tenant exactly like `audit_log`: a query role assuming app_tenant sees only its own
-- tenant's rows; the trusted pool-owner connection the server runs as bypasses RLS to write any
-- tenant's row (and the server-wide NULL-tenant ones) and to read the fleet-wide list. Forward-only
-- and additive, applied idempotently on every boot (ADR-0017).
CREATE TABLE IF NOT EXISTS alerts (
    id              text   NOT NULL PRIMARY KEY,
    tenant_id       text,
    kind            text   NOT NULL,
    dedup_key       text   NOT NULL DEFAULT '',
    severity        text   NOT NULL,
    summary         text   NOT NULL,
    detail          jsonb  NOT NULL,
    first_seen_at   bigint NOT NULL,
    last_seen_at    bigint NOT NULL,
    resolved_at     bigint,
    acknowledged_at bigint
);
-- At most one *open* alert per condition. `coalesce(tenant_id, '')` folds the server-wide NULL-tenant
-- alerts into the same uniqueness rule; the partial predicate lets a resolved alert of the same key
-- coexist as history while only the live one is constrained. This is the ON CONFLICT target the
-- adapter's open-or-refresh upsert names.
CREATE UNIQUE INDEX IF NOT EXISTS alerts_open_key
    ON alerts (coalesce(tenant_id, ''), kind, dedup_key)
    WHERE resolved_at IS NULL;
-- The console reads a tenant's alerts newest-first; the active list drops resolved rows.
CREATE INDEX IF NOT EXISTS alerts_by_tenant_last_seen ON alerts (tenant_id, last_seen_at DESC);
CREATE INDEX IF NOT EXISTS alerts_active
    ON alerts (last_seen_at DESC) WHERE resolved_at IS NULL;

ALTER TABLE alerts ENABLE ROW LEVEL SECURITY;
DROP POLICY IF EXISTS alerts_tenant_isolation ON alerts;
CREATE POLICY alerts_tenant_isolation ON alerts
    USING (tenant_id = current_setting('app.tenant_id', true));
-- An alert has a lifecycle (refresh, resolve, acknowledge), so UPDATE is granted here where the
-- append-only audit log withholds it. No DELETE: a resolved alert is history, kept until retention.
GRANT SELECT, INSERT, UPDATE ON alerts TO app_tenant;
