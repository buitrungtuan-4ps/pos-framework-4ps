-- Copyright (c) 2026 Pizza 4P's. All rights reserved.
-- Proprietary and confidential. Internal use only. See LICENSE.

-- The index the archive-retention sweep walks
-- ([ADR-0124](../../../../docs/adr/0124-a-store-that-can-be-restored.md), slice 3).
--
-- WHY 0062'S INDEX IS NOT THE ONE
--
-- Migration 0062 added `store_archives_by_age ON (tenant_id, taken_at)` for "what is older than
-- the window", and that is the right index for a *tenant's* question. The retention sweep is not
-- a tenant's question: it runs as the cloud, over every tenant at once, exactly as the subject
-- masking cron does (0004, ADR-0035) — one pass finding everything past the window rather than a
-- loop over the tenant list, which would be a query per tenant per sweep and would miss a tenant
-- created between two iterations.
--
-- With `tenant_id` leading, that scan cannot use the index at all: the sweep's predicate is
-- `taken_at <= $1` with no tenant, so PostgreSQL would sequentially scan the whole table every
-- sweep. This index leads with `taken_at` and serves it directly. 0062's index stays: it is the
-- one the console's per-store read uses, and dropping it to save a little space would be trading
-- the interactive query for the nightly one.
--
-- WHY THE SWEEP EXISTS AT ALL, WHICH IS NOT DISK
--
-- Disk is the cheap reason. The real one is that a store's archive is its whole database, and a
-- store database carries the buyer name and tax code a B2B invoice needs (ADR-0107) — T1 data.
-- The subject-masking cron redacts that data in the *live* store on a legal retention period, and
-- an archive taken before the masking preserves what the masking removed. Capping the archive
-- window strictly below the subject window is what makes the redaction eventually true everywhere
-- instead of true only in the row somebody looked at: past the archive window, the last copy that
-- still held the buyer's details is gone. `pos_cloud`'s configuration refuses to start when the
-- two windows are the wrong way round, so this is enforced rather than documented.
--
-- Forward-only and additive: an index, nothing else. Applied idempotently on every boot (ADR-0017).

-- "What, anywhere, is past the archive window" — the retention sweep's only predicate.
CREATE INDEX IF NOT EXISTS store_archives_by_taken_at
    ON store_archives (taken_at);
