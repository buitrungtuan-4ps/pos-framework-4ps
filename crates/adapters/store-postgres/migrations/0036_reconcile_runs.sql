-- Copyright (c) 2026 Pizza 4P's. All rights reserved.
-- Proprietary and confidential. Internal use only. See LICENSE.

-- Reconciliation run history ([ADR-0078](../../../../docs/adr/0078-sync-and-ota-closure.md), Track
-- O3). The `POST /internal/reconcile` diff ([ADR-0040](../../../../docs/adr/0040-reconciliation.md))
-- answered "which of these ids am I missing?" statelessly, so a gap it closed was invisible after the
-- fact — nothing recorded that reconciliation ran, for which store, or how much it caught. This table
-- is the trail: one append-only row per diff, so `GET /admin/reconcile` can show the console that
-- reconciliation happened and what it found.
--
--   * `run_id`             — a ULID string, so ordering by it is chronological.
--   * `candidates_offered` — how many ids the edge offered in the manifest (the window it reconciled).
--   * `missing_found`      — how many of them the cloud was missing (the ids it asked the edge to
--                            re-push); zero means the store was fully in sync.
--   * `ran_at`             — Unix ms of the diff, stamped from the server clock.
--
-- Operational telemetry only: counts and a timestamp, never event contents or a customer identifier
-- (a reconciliation run is device/store bookkeeping, kept out of the T1/T2 reproduction rules). Rows
-- are never updated or deleted here — history is append-only; retention prunes it elsewhere if ever
-- needed.
--
-- Keyed and RLS-isolated by tenant, exactly as `store_liveness` (0020) and `config_trees` are: a
-- store's reconciliation history is its tenant's data. The trusted admin connection (the pool owner
-- the server runs as) bypasses RLS to record any tenant's run and to read across tenants for the
-- console; a query role assuming `app_tenant` is filtered to its own. Forward-only and additive,
-- applied idempotently on every boot (ADR-0017). Greenfield — no backfill.
CREATE TABLE IF NOT EXISTS reconcile_runs (
    run_id             text   PRIMARY KEY,
    tenant_id          text   NOT NULL,
    store_id           text   NOT NULL,
    candidates_offered integer NOT NULL,
    missing_found      integer NOT NULL,
    ran_at             bigint NOT NULL
);

CREATE INDEX IF NOT EXISTS reconcile_runs_by_store
    ON reconcile_runs (tenant_id, store_id, ran_at DESC);

ALTER TABLE reconcile_runs ENABLE ROW LEVEL SECURITY;
DROP POLICY IF EXISTS reconcile_runs_tenant_isolation ON reconcile_runs;
CREATE POLICY reconcile_runs_tenant_isolation ON reconcile_runs
    USING (tenant_id = current_setting('app.tenant_id', true));
GRANT SELECT, INSERT ON reconcile_runs TO app_tenant;
