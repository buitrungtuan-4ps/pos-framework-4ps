-- Copyright (c) 2026 Pizza 4P's. All rights reserved.
-- Proprietary and confidential. Internal use only. See LICENSE.

-- Background-task health ([ADR-0068](../../../../docs/adr/0068-fleet-liveness.md), Track O1 slice 4).
-- One row per named background loop the cloud runs — the rollup projector, the retention/PII sweep,
-- the webhook dispatcher. Each loop upserts its row at the end of every tick with the contact instant
-- and a small JSON detail of what that tick did (whether it succeeded, its configured interval, a
-- count or two). The console reads it to answer "are the background workers alive and keeping up?" —
-- staleness is derived at read time (`now - last_tick_at` against the interval the row itself records),
-- exactly as store liveness derives online/offline, so nothing writes a "stalled" flag.
--
--   * `task`         — the stable loop name (e.g. `rollup_projector`, `retention`, `webhook_dispatcher`).
--   * `last_tick_at` — Unix ms of the loop's most recent completed iteration.
--   * `detail`       — the tick's self-describing summary as jsonb (`{"ok":bool,"interval_secs":N,…}`),
--                      so the reader needs no side channel to judge freshness or success.
--
-- Fleet-wide *server* state, not tenant data: these loops run once per cloud, not per tenant. So, like
-- `super_admin` (0003) and `tenants` (0011), it has no parent tenant to scope by — no RLS, no
-- `app_tenant` grant — and is administered only by the trusted pool-owner connection the server runs
-- as. Forward-only and additive, applied idempotently on every boot (ADR-0017).
CREATE TABLE IF NOT EXISTS task_health (
    task         text        PRIMARY KEY,
    last_tick_at bigint      NOT NULL,
    detail       jsonb       NOT NULL DEFAULT '{}'::jsonb,
    updated_at   timestamptz NOT NULL DEFAULT now()
);
