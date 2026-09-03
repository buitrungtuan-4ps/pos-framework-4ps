-- Copyright (c) 2026 Pizza 4P's. All rights reserved.
-- Proprietary and confidential. Internal use only. See LICENSE.

-- An index covering the audit trail's whole order, so a page of it is cheap and its total is
-- index-only (ADR-0098 decision 9, F2 item B3 slice 2).
--
-- `audit_log` is the one paged table whose read was already ordered *totally*: `at DESC, id DESC`,
-- and `id` is the primary key, so there is no tie for `LIMIT`/`OFFSET` to break differently between
-- one page and the next. Nothing about the query changes here.
--
-- What is missing is the tiebreaker in the index. `0022_audit_log.sql` created
-- `audit_log_by_tenant_at (tenant_id, at DESC)`, which finds a tenant's rows in `at` order but
-- stops short of `id` — so the plan grows a `Sort` node to finish the order, and `LIMIT` then
-- truncates a completed sort instead of stopping a scan.
--
-- The count matters more here than on any other paged read. `count(*) OVER()` cannot stop early: it
-- walks every row matching the filters, on every page the operator turns. With the sort columns in
-- the index that walk is index-only — no heap fetch per row — which is the difference between
-- counting a tenant's audit history and reading it.
--
-- Why a second index rather than replacing the first: `CREATE INDEX` is additive and `DROP INDEX` is
-- not (ADR-0017 is forward-only), and the two-column index still serves the per-entity panel's read.
-- The overlap costs one index's worth of writes on a table written once per console mutation — a
-- rate set by how fast a human clicks.
--
-- Forward-only and additive, applied idempotently on every boot (ADR-0017). No data change.

CREATE INDEX IF NOT EXISTS audit_log_by_tenant_newest
    ON audit_log (tenant_id, at DESC, id DESC);
