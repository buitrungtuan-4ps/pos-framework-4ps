-- Copyright (c) 2026 Pizza 4P's. All rights reserved.
-- Proprietary and confidential. Internal use only. See LICENSE.

-- Store groups and the batches published to them
-- ([ADR-0122](../../../../docs/adr/0122-a-store-group-is-a-delivery-cohort.md)).
--
-- WHAT THIS IS FOR
--
-- The catalog data is already shared: `catalog_placements` (0012) has no `store_id` column at all,
-- so one row prices an item for the whole tenant. What is per-store is the *config tree* —
-- `config_trees` (0004) is `PRIMARY KEY (tenant_id, store_id)` — and delivery follows the tree, so
-- publishing one menu to fifty shops was fifty publishes done by hand. These four tables are the
-- cohort an operator publishes to, and the durable record of what each publish did at each store.
--
-- WHY THIS IS NOT A FIFTH CONFIG LAYER
--
-- ADR-0122 §2 rejected a shared `Group` layer inside the config tree, and nothing here is one. A
-- group holds no configuration: a batch publish writes the same document into *N* per-store trees,
-- exactly as *N* individual publishes would have. Composition stays a single-row read on the store's
-- sync hot path, no store's effective document changes, and dropping every row in `store_groups`
-- returns the trees byte-for-byte to what they are without it.
--
-- THE FOUR TABLES
--
--   * `store_groups`        — a named, flat, tenant-scoped cohort. Not the brand: `stores.brand_id`
--                             (0011) already exists and is *identity* (which sign is over the door),
--                             one per store, whereas the sets an operator publishes to cut across it
--                             — the airport branches on a reduced menu, the shops on one tax
--                             registration, the pilots that take a change first. A store belongs to
--                             one brand and any number of groups.
--   * `store_group_members` — the membership set. No `FOREIGN KEY` to `stores`, matching the rest of
--                             this schema: the registry (0011) and the config tree (0004) are
--                             likewise keyed by id without cross-table constraints, because a
--                             tenant's tables are RLS-partitioned and a foreign key across them buys
--                             nothing the application does not already check. A membership row for a
--                             store that was archived is harmless — the batch skips it and says so.
--   * `config_batches`      — one row per batch: what was published, to which group, by whom, and
--                             when it started and finished. `finished_at` stays NULL for a batch
--                             that died mid-fan-out, which is exactly the row an operator needs to
--                             find afterwards.
--   * `config_batch_results`— one row per `(batch, store)`, carrying the outcome. **Three** outcomes
--                             and not two: `applied`, `skipped` (a prerequisite was missing, or the
--                             store is archived — deliberately protected), and `failed` (the publish
--                             was attempted and refused). A two-state report files a store that was
--                             protected under the same heading as one that broke, which is the
--                             report going wrong rather than the batch.
--
-- WHY THERE IS NO ROLLBACK COLUMN, AND WHY A BATCH IS NOT ATOMIC
--
-- Each store's tree is its own row, so an all-or-nothing batch would hold *N* row locks for the
-- length of the fan-out and fail as "nothing happened, at 200 shops, because of one" (ADR-0122 §5).
-- A partial success is the honest outcome and a recoverable one: re-running is idempotent for the
-- stores that already succeeded, because a publish producing an identical document is a no-op
-- version. What must not be partial is the *reporting*, which is what `config_batch_results` is.
--
-- THE 200-MEMBER CAP LIVES IN THE ROUTE, NOT HERE
--
-- ADR-0122 §8 caps a group at 200 stores so a synchronous batch stays inside one request.
-- Expressing that as a constraint would need a row-counting trigger — machinery, on the write path,
-- to restate a number the handler checks anyway. `docs/capacity-and-reliability.md` models tenants
-- up to 1,000 stores; such a tenant uses several groups, which is how an operator of that size
-- already thinks.
--
-- CLASSIFICATION
--
-- A group is a name and a list of store ids; a batch is what an admin published and when. T3
-- (Internal) operational metadata — no customer or employee identifier, and `actor_email` is the
-- console admin who pressed the button, the same identity `audit_log` (0021) already records for
-- every other write.
--
-- Tenant-scoped exactly like the rest (0012/0028/0032/0037/0060): RLS on `app.tenant_id`, a grant to
-- `app_tenant`, the trusted pool owner bypassing RLS. Forward-only and additive, applied
-- idempotently on every boot (ADR-0017). Greenfield — a tenant with no groups behaves exactly as it
-- does today.

CREATE TABLE IF NOT EXISTS store_groups (
    tenant_id  text        NOT NULL,
    group_id   text        NOT NULL,
    name       text        NOT NULL,
    status     text        NOT NULL DEFAULT 'active',
    created_at timestamptz NOT NULL DEFAULT now(),
    updated_at timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (tenant_id, group_id),
    CONSTRAINT store_groups_status CHECK (status IN ('active', 'archived'))
);

CREATE TABLE IF NOT EXISTS store_group_members (
    tenant_id text NOT NULL,
    group_id  text NOT NULL,
    store_id  text NOT NULL,
    PRIMARY KEY (tenant_id, group_id, store_id)
);

-- "Which groups is this store in" — the reverse of the primary key, for the Stores screen.
CREATE INDEX IF NOT EXISTS store_group_members_by_store
    ON store_group_members (tenant_id, store_id);

CREATE TABLE IF NOT EXISTS config_batches (
    tenant_id   text        NOT NULL,
    batch_id    text        NOT NULL,
    group_id    text        NOT NULL,
    node        text        NOT NULL,
    arguments   jsonb       NOT NULL,
    actor_email text        NOT NULL,
    -- Milliseconds since the epoch, like `audit_log.at` (0021) and unlike the `timestamptz` columns
    -- above: these two are supplied by `pos-cloud`'s `ClockSource`, so the fake and the real adapter
    -- agree on the clock, and a test can pin it. `created_at`/`updated_at` stay `timestamptz
    -- DEFAULT now()` because nothing reads them back — they are for a human with `psql`.
    started_at  bigint      NOT NULL,
    finished_at bigint,
    PRIMARY KEY (tenant_id, batch_id)
);

-- A group's batches, newest first: the history panel on the group screen.
CREATE INDEX IF NOT EXISTS config_batches_by_group
    ON config_batches (tenant_id, group_id, started_at DESC);

CREATE TABLE IF NOT EXISTS config_batch_results (
    tenant_id  text        NOT NULL,
    batch_id   text        NOT NULL,
    store_id   text        NOT NULL,
    outcome    text        NOT NULL,
    -- The refusal's own message, for `skipped` and `failed`. NULL when it applied: a success has
    -- nothing to say that `version_id` does not already say better.
    detail     text,
    -- The config version the publish produced, for `applied`. This is what makes drift visible: a
    -- member whose last batch produced no version is not running what its group runs.
    version_id text,
    at         bigint      NOT NULL,
    PRIMARY KEY (tenant_id, batch_id, store_id),
    CONSTRAINT config_batch_results_outcome
        CHECK (outcome IN ('applied', 'skipped', 'failed'))
);

-- "What did the last batch do at this store" — the drift read, per store rather than per batch.
CREATE INDEX IF NOT EXISTS config_batch_results_by_store
    ON config_batch_results (tenant_id, store_id, at DESC);

ALTER TABLE store_groups ENABLE ROW LEVEL SECURITY;
DROP POLICY IF EXISTS store_groups_tenant_isolation ON store_groups;
CREATE POLICY store_groups_tenant_isolation ON store_groups
    USING (tenant_id = current_setting('app.tenant_id', true));
GRANT SELECT, INSERT, UPDATE, DELETE ON store_groups TO app_tenant;

ALTER TABLE store_group_members ENABLE ROW LEVEL SECURITY;
DROP POLICY IF EXISTS store_group_members_tenant_isolation ON store_group_members;
CREATE POLICY store_group_members_tenant_isolation ON store_group_members
    USING (tenant_id = current_setting('app.tenant_id', true));
GRANT SELECT, INSERT, UPDATE, DELETE ON store_group_members TO app_tenant;

ALTER TABLE config_batches ENABLE ROW LEVEL SECURITY;
DROP POLICY IF EXISTS config_batches_tenant_isolation ON config_batches;
CREATE POLICY config_batches_tenant_isolation ON config_batches
    USING (tenant_id = current_setting('app.tenant_id', true));
GRANT SELECT, INSERT, UPDATE, DELETE ON config_batches TO app_tenant;

ALTER TABLE config_batch_results ENABLE ROW LEVEL SECURITY;
DROP POLICY IF EXISTS config_batch_results_tenant_isolation ON config_batch_results;
CREATE POLICY config_batch_results_tenant_isolation ON config_batch_results
    USING (tenant_id = current_setting('app.tenant_id', true));
GRANT SELECT, INSERT, UPDATE, DELETE ON config_batch_results TO app_tenant;
