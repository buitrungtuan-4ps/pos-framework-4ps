-- Copyright (c) 2026 Pizza 4P's. All rights reserved.
-- Proprietary and confidential. Internal use only. See LICENSE.

-- Modifier groups for the cloud catalog (Phase 2a, ADR-0066 entities 4 and 5): a set of modifier
-- choices with a min/max selection rule, attached to items. A modifier is itself an item (ADR-0063 —
-- a menu_item_id priced in the same money), so there is no separate modifier entity: `member_item_ids`
-- are the items offered as choices and `attached_item_ids` are the items this group modifies, both
-- stored as `jsonb` arrays of ULID strings (read/written as one opaque document via the text::jsonb
-- cast the config tree uses; the seam serialises the id list in and back).
--
-- This is authoring only — the compiled MenuEntry carries no modifier reference yet; wiring modifiers
-- to the edge is a pos-proto/ADR-0063 extension and its own resolver slice. Tenant-scoped exactly like
-- the rest of the catalog: RLS on `app.tenant_id`, a grant to `app_tenant`, the trusted pool owner
-- bypassing RLS. Forward-only and additive, applied idempotently on every boot (ADR-0017). Greenfield.

CREATE TABLE IF NOT EXISTS catalog_modifier_groups (
    modifier_group_id text        PRIMARY KEY,
    tenant_id         text        NOT NULL,
    name              text        NOT NULL,
    min_select        integer     NOT NULL DEFAULT 0,
    max_select        integer     NOT NULL DEFAULT 0,
    member_item_ids   jsonb       NOT NULL DEFAULT '[]'::jsonb,
    attached_item_ids jsonb       NOT NULL DEFAULT '[]'::jsonb,
    status            text        NOT NULL DEFAULT 'active',
    created_at        timestamptz NOT NULL DEFAULT now(),
    updated_at        timestamptz NOT NULL DEFAULT now(),
    CONSTRAINT catalog_modifier_groups_status CHECK (status IN ('active', 'archived'))
);
CREATE INDEX IF NOT EXISTS catalog_modifier_groups_by_tenant
    ON catalog_modifier_groups (tenant_id);

ALTER TABLE catalog_modifier_groups ENABLE ROW LEVEL SECURITY;
DROP POLICY IF EXISTS catalog_modifier_groups_tenant_isolation ON catalog_modifier_groups;
CREATE POLICY catalog_modifier_groups_tenant_isolation ON catalog_modifier_groups
    USING (tenant_id = current_setting('app.tenant_id', true));
GRANT SELECT, INSERT, UPDATE ON catalog_modifier_groups TO app_tenant;
