-- Copyright (c) 2026 Pizza 4P's. All rights reserved.
-- Proprietary and confidential. Internal use only. See LICENSE.

-- Media renditions (Track M5, ADR-0075). The ADR-0042 image pipeline produces two JPEG renditions
-- under hard byte budgets (a ≤30 KB thumbnail and a ≤150 KB detail); this is where they live. Per
-- ADR-0042 and ADR-0031 (object storage exists only for Litestream and is scheduled for deletion),
-- renditions are stored as Postgres `bytea` here, not in the blob-garage port — the one durable store
-- the cloud already backs up.
--
-- A row is one uploaded image, keyed by a minted `MediaId` (ULID). Only the two bounded renditions are
-- kept; the original upload is never persisted (it is the unbounded attack surface the pipeline exists
-- to contain). `content_type` is `image/jpeg` today. `detail_bytes` is the detail rendition's size,
-- carried so a listing can show it without shipping the bytea. Items and brands reference a row by id
-- (`image_ref`, a later slice); a dangling reference serves a placeholder, never an error.
--
-- Media is immutable: a change is a new upload plus a delete, never an UPDATE — so `app_tenant` holds
-- SELECT/INSERT/DELETE but not UPDATE. Tenant-scoped exactly like the rest of the cloud (0012/0028):
-- RLS on `app.tenant_id`, the trusted pool owner bypassing RLS. Forward-only and additive, applied
-- idempotently on every boot (ADR-0017). Greenfield — no backfill.

CREATE TABLE IF NOT EXISTS media_assets (
    media_id     text        PRIMARY KEY,
    tenant_id    text        NOT NULL,
    content_type text        NOT NULL,
    thumbnail    bytea       NOT NULL,
    detail       bytea       NOT NULL,
    detail_bytes integer     NOT NULL,
    created_at   timestamptz NOT NULL DEFAULT now(),
    CONSTRAINT media_assets_detail_bytes CHECK (detail_bytes >= 0)
);
CREATE INDEX IF NOT EXISTS media_assets_by_tenant ON media_assets (tenant_id, created_at DESC);

ALTER TABLE media_assets ENABLE ROW LEVEL SECURITY;
DROP POLICY IF EXISTS media_assets_tenant_isolation ON media_assets;
CREATE POLICY media_assets_tenant_isolation ON media_assets
    USING (tenant_id = current_setting('app.tenant_id', true));
GRANT SELECT, INSERT, DELETE ON media_assets TO app_tenant;
