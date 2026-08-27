-- Copyright (c) 2026 Pizza 4P's. All rights reserved.
-- Proprietary and confidential. Internal use only. See LICENSE.

-- Fleet liveness: the smallest durable record of whether a store is actually there
-- ([ADR-0068](../../../../docs/adr/0068-fleet-liveness.md), Track O1). One row per (tenant, store),
-- upserted from the store's config pull (`GET /sync/stores/{id}/config`, ADR-0033) — the signal the
-- edge already sends and the handler used to discard. Derived and mutable (upserted on every pull),
-- so it is a read model for the console, deliberately separate from `config_trees`: a store is "seen"
-- whether or not it has a published config tree, and the later heartbeat reports liveness with no
-- config pull at all, so liveness must not hang off a row that may not exist. Forward-only and
-- additive, applied idempotently on every boot (ADR-0017).
--
--   * `last_seen_at`        — Unix ms of the store's most recent contact (pull or, later, heartbeat).
--   * `config_version_held` — the config version the edge reported holding on its last pull, or NULL
--                             (a fresh store that has pulled but holds nothing yet). A ULID string,
--                             compared at read time against the store's currently-published version.
--   * `last_config_pull_at` — Unix ms of the most recent *config pull* specifically, so a later
--                             heartbeat (which advances `last_seen_at` only) stays distinguishable
--                             from a genuine config sync.
--
-- Online/offline is NOT stored — there is no event when a store goes quiet — it is derived at read
-- time from `now - last_seen_at` against a threshold the fleet read owns.
--
-- Keyed and RLS-isolated by tenant, exactly as `config_trees` is: the store's liveness is its
-- tenant's data. The trusted admin connection (the pool owner the server runs as) bypasses RLS to
-- upsert any tenant's row on a pull and to read across tenants for the fleet console; a query role
-- assuming app_tenant is filtered to its own.
CREATE TABLE IF NOT EXISTS store_liveness (
    tenant_id           text   NOT NULL,
    store_id            text   NOT NULL,
    last_seen_at        bigint NOT NULL,
    config_version_held text,
    last_config_pull_at bigint,
    PRIMARY KEY (tenant_id, store_id)
);

ALTER TABLE store_liveness ENABLE ROW LEVEL SECURITY;
DROP POLICY IF EXISTS store_liveness_tenant_isolation ON store_liveness;
CREATE POLICY store_liveness_tenant_isolation ON store_liveness
    USING (tenant_id = current_setting('app.tenant_id', true));
GRANT SELECT, INSERT, UPDATE ON store_liveness TO app_tenant;
