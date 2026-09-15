-- Copyright (c) 2026 Pizza 4P's. All rights reserved.
-- Proprietary and confidential. Internal use only. See LICENSE.

-- Releases: one decision, many writes (Wave 4 · PR-7, ADR-0125). A Tết menu is not one node — it is a
-- menu, the tax rates it prices against, the campaigns that discount it, the reason codes the staff
-- void it with, and the layout that shows it, all switching on together at forty stores. Today that is
-- eighty separate publishes with no name above them, no report across them, and no way to cancel the
-- set (findings F8 / F9 / F10).
--
-- A release is **bookkeeping over writes that already had a home**: creating one expands to one
-- `scheduled_publishes` row per (node, store) pair, each carrying this release's id, and ADR-0077's
-- existing activator applies them. There is no second activator and no second way for a config version
-- to be born — see ADR-0125 §1.
--
-- The moment is stored twice on purpose. `wall_clock_*` is what the operator said ("Monday 04:00,
-- local"); the per-store instant it resolved to lives on each `scheduled_publishes.effective_at`,
-- because "Monday 04:00 at each store" is one instant per timezone and a fleet spanning Ho Chi Minh
-- City and Tokyo has a two-hour spread (finding F17 / decision D8, option O2). Conversion happens on
-- the cloud at schedule time, from each store's published `locale.timezone`, so the operator can be
-- *shown* the forty instants they are committing to. A release given a plain instant instead leaves
-- the wall-clock columns null and needs no locale at all.
--
-- `target_group_id` records the cohort a release was aimed at, for the report — but the rows are per
-- store, because the group is expanded at schedule time (ADR-0125 §3). A store added to the cohort on
-- Friday must not silently receive a Monday menu nobody checked it against.
--
-- `status` is the derived roll-up of the pairs, stored so a list read does not have to aggregate:
-- DRAFT → SCHEDULED → APPLYING → APPLIED | PARTIAL. PARTIAL is the state the whole record exists for —
-- N × M writes fail individually, and at forty stores an operator needs the two shops that did not take
-- it, not a red cross over the fleet (ADR-0125 §4). There is no ROLLED_BACK: an applied write is a
-- config version, and the way back is ADR-0095's version history.
--
-- Forward-only and additive, applied idempotently on every boot (ADR-0017). Greenfield — no backfill;
-- `scheduled_publishes.release_id` is nullable precisely so ADR-0077's per-store campaign schedule keeps
-- working unchanged, and a row with no release id is exactly today's behaviour.

CREATE TABLE IF NOT EXISTS releases (
    id              text        NOT NULL,
    tenant_id       text        NOT NULL,
    name            text        NOT NULL,
    status          text        NOT NULL DEFAULT 'DRAFT',
    -- The cohort this release was aimed at, or null for an explicit store list. Recorded for the
    -- report; the concrete stores live on the scheduled_publishes rows.
    target_group_id text,
    -- What the operator said, when they said it in local terms. Null for an instant release.
    wall_clock_date text,
    wall_clock_time text,
    -- The single instant, for a release scheduled in UTC rather than in each store's own clock. Null
    -- for a wall-clock release, whose instants are per store.
    instant_at      timestamptz,
    created_by      text        NOT NULL,
    created_at      timestamptz NOT NULL DEFAULT now(),
    updated_at      timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (tenant_id, id)
);
-- The console's list: a tenant's releases, newest first.
CREATE INDEX IF NOT EXISTS releases_by_tenant
    ON releases (tenant_id, created_at DESC);
-- The activator's roll-up: the releases still in flight, fleet-wide.
CREATE INDEX IF NOT EXISTS releases_in_flight
    ON releases (status)
    WHERE status IN ('SCHEDULED', 'APPLYING');

ALTER TABLE releases ENABLE ROW LEVEL SECURITY;
DROP POLICY IF EXISTS releases_tenant_isolation ON releases;
CREATE POLICY releases_tenant_isolation ON releases
    USING (tenant_id = current_setting('app.tenant_id', true));
GRANT SELECT, INSERT, UPDATE, DELETE ON releases TO app_tenant;

-- The pair rows join their release. Nullable: ADR-0077's own schedule route writes rows with no
-- release, and those keep working untouched.
ALTER TABLE scheduled_publishes
    ADD COLUMN IF NOT EXISTS release_id text;
-- The report's hot path: every pair of one release, for the node × store grid.
CREATE INDEX IF NOT EXISTS scheduled_publishes_by_release
    ON scheduled_publishes (tenant_id, release_id)
    WHERE release_id IS NOT NULL;

-- Why a pair carries its own failure rather than the release carrying a list: the outcome of a publish
-- already lives on the row that made it (`applied_version_id`), and a failure is the same fact in the
-- negative. A release's PARTIAL is then derived from its pairs rather than asserted beside them, so the
-- two cannot disagree.
ALTER TABLE scheduled_publishes
    ADD COLUMN IF NOT EXISTS failure text;
