-- Copyright (c) 2026 Pizza 4P's. All rights reserved.
-- Proprietary and confidential. Internal use only. See LICENSE.

-- An index covering the media library's *total* order, so a page of it is both correct and cheap
-- (ADR-0098 decision 9, F2 item B3 slice 2).
--
-- `0030_media_assets.sql` created `media_assets_by_tenant (tenant_id, created_at DESC)`, and until now the
-- read ordered by `created_at DESC` alone — which that index serves. Paging cannot use that order.
--
-- `created_at` defaults to `now()`, and in PostgreSQL `now()` is *transaction* time: every row one
-- transaction writes carries the identical timestamp, not merely a close one. Measured on a real
-- database, six `media_assets` rows inserted in one transaction produced **one** distinct
-- `created_at`. `ORDER BY created_at DESC` therefore imposes no order at all across such a batch, and
-- `LIMIT`/`OFFSET` over a non-total order may return a row on two pages or on neither — the database
-- is free to break the tie differently between the query that builds page one and the query that
-- builds page two. That is a correctness failure, and it hides because a given plan usually happens
-- to be stable.
--
-- So the read now orders `created_at DESC, media_id DESC` — total, since `media_id` is the primary
-- key — and this index carries the whole of it. Without the tiebreaker in the index the plan grows a
-- `Sort` node again and `LIMIT` truncates a finished sort instead of stopping a scan, which is the
-- economy migration 0040 bought for vouchers.
--
-- Why a second index rather than replacing the first: `CREATE INDEX` is additive and `DROP INDEX` is
-- not (ADR-0017 is forward-only), and the two-column index still serves any read that filters by
-- tenant without the tiebreaker. The overlap costs one index's worth of writes on a table written
-- once per upload.
--
-- Forward-only and additive, applied idempotently on every boot (ADR-0017). No data change.

CREATE INDEX IF NOT EXISTS media_assets_by_tenant_newest
    ON media_assets (tenant_id, created_at DESC, media_id DESC);
