-- Copyright (c) 2026 Pizza 4P's. All rights reserved.
-- Proprietary and confidential. Internal use only. See LICENSE.

-- Scheduled, effective-dated config publishes (Track M3, ADR-0077). Every other publish is immediate;
-- a Tết menu or a midnight price change needs it to switch on *then*, without a human awake at 00:00.
-- A row holds a snapshot of a node value, its target (store, node key), and an effective_at; a
-- background activator applies it at its time through the same config tree the immediate publishes use.
--
-- Snapshot-at-schedule: `node_value` is what was authored and reviewed when the publish was scheduled,
-- not recomputed at fire time, so later edits never leak into a publish nobody looked at again. The
-- mechanism is node-agnostic — `node_key` names any Store-layer key (`campaigns`, `menu`, `tax`, …).
-- `applied_version_id` records the config version the activator produced.
--
-- The activator reads `status = 'PENDING' AND effective_at <= now()` across all tenants as the trusted
-- pool owner (RLS bypassed, like the rollup projector's fleet scan); the console's per-store list and
-- the schedule/cancel writes are tenant-scoped. RLS on `app.tenant_id` is the second line. Forward-only
-- and additive, applied idempotently on every boot (ADR-0017). Greenfield — no backfill.

CREATE TABLE IF NOT EXISTS scheduled_publishes (
    id                 text        NOT NULL,
    tenant_id          text        NOT NULL,
    store_id           text        NOT NULL,
    node_key           text        NOT NULL,
    node_value         jsonb       NOT NULL,
    effective_at       timestamptz NOT NULL,
    status             text        NOT NULL DEFAULT 'PENDING',
    created_by         text        NOT NULL,
    created_at         timestamptz NOT NULL DEFAULT now(),
    applied_version_id text,
    PRIMARY KEY (tenant_id, id)
);
-- The activator's hot path: the pending rows whose time has come, fleet-wide.
CREATE INDEX IF NOT EXISTS scheduled_publishes_due
    ON scheduled_publishes (effective_at)
    WHERE status = 'PENDING';
CREATE INDEX IF NOT EXISTS scheduled_publishes_by_store
    ON scheduled_publishes (tenant_id, store_id);

ALTER TABLE scheduled_publishes ENABLE ROW LEVEL SECURITY;
DROP POLICY IF EXISTS scheduled_publishes_tenant_isolation ON scheduled_publishes;
CREATE POLICY scheduled_publishes_tenant_isolation ON scheduled_publishes
    USING (tenant_id = current_setting('app.tenant_id', true));
GRANT SELECT, INSERT, UPDATE, DELETE ON scheduled_publishes TO app_tenant;
