-- Copyright (c) 2026 Pizza 4P's. All rights reserved.
-- Proprietary and confidential. Internal use only. See LICENSE.

-- The campaign authoring table (Track M3, ADR-0077). Where an operator's promotions live between
-- edits, before a publish assembles them into the `campaigns` config node the edge's pricing engine
-- (pos_core::campaign, docs/pos-spec.md §7) evaluates. Until M3 the engine was finished but had no
-- inputs: `Campaign` is a pure runtime type with no wire form, so there was nowhere to author a
-- promotion.
--
-- One row per (tenant, campaign). The `campaign` column is the whole authored campaign — the wire
-- `PublishedCampaign` (id, name, kind, priority, exclusion group, action, conditions incl. the weekly
-- window, quota) — held as `jsonb`, the same store-the-shape-as-a-document choice config_trees and the
-- rollups make: a campaign is a small, nested object an operator edits as a unit, not a grid of
-- scalars, so a document beats a wide table of nullable columns. The id is a ULID string, so ordering
-- by it is creation order. Campaign pricing terms are T2 (Confidential): this is configuration, and the
-- row carries no customer identifier — a voucher is a code, not a person.
--
-- Tenant-scoped exactly like the rest of the config data (0012/0028): RLS on `app.tenant_id`, a grant
-- to `app_tenant`, the trusted pool owner bypassing RLS. CRUD is per-campaign (an operator edits one
-- promotion at a time), so `app_tenant` holds INSERT/UPDATE/DELETE as well as SELECT. Forward-only and
-- additive, applied idempotently on every boot (ADR-0017). Greenfield — no backfill.

CREATE TABLE IF NOT EXISTS campaigns (
    tenant_id   text        NOT NULL,
    campaign_id text        NOT NULL,
    campaign    jsonb       NOT NULL,
    updated_at  timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (tenant_id, campaign_id)
);
CREATE INDEX IF NOT EXISTS campaigns_by_tenant ON campaigns (tenant_id);

ALTER TABLE campaigns ENABLE ROW LEVEL SECURITY;
DROP POLICY IF EXISTS campaigns_tenant_isolation ON campaigns;
CREATE POLICY campaigns_tenant_isolation ON campaigns
    USING (tenant_id = current_setting('app.tenant_id', true));
GRANT SELECT, INSERT, UPDATE, DELETE ON campaigns TO app_tenant;
