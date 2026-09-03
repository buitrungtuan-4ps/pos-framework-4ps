-- Copyright (c) 2026 Pizza 4P's. All rights reserved.
-- Proprietary and confidential. Internal use only. See LICENSE.

-- An index that serves the voucher list's own sort order, so a page of it is cheap and not merely
-- small (ADR-0098, F2 item B3 slice 1).
--
-- `0033_vouchers.sql` created `vouchers_by_campaign (tenant_id, campaign_id)`, which finds a
-- campaign's rows. But the read orders by `created_at DESC, voucher_id DESC`, and that index carries
-- neither column, so PostgreSQL fetches every matching row and sorts the lot. Unpaged that cost was
-- unavoidable — the caller wanted all of them anyway. Paged it would be absurd: `LIMIT 25` would
-- shrink the response while the database still sorted 30 000 codes, on every page of the pager. So
-- the paged read arrives with its sort in an index, in the same change as the SQL that needs it.
--
-- Why a second index rather than replacing the first: `CREATE INDEX` is additive and `DROP INDEX` is
-- not (ADR-0017 is forward-only), and the two-column index still serves any read that filters
-- without sorting. The overlap costs one index's worth of writes on a table written in batches at
-- promotion time and read by a console — the wrong side of that trade to optimise.
--
-- The column order is the query's: equality columns first (`tenant_id`, `campaign_id`), then the
-- sort columns in the direction the read asks for. With that, the index walk *is* the sort, and
-- `LIMIT` stops the scan instead of truncating a finished one.
--
-- Forward-only and additive, applied idempotently on every boot (ADR-0017). No data change.

CREATE INDEX IF NOT EXISTS vouchers_by_campaign_newest
    ON vouchers (tenant_id, campaign_id, created_at DESC, voucher_id DESC);
