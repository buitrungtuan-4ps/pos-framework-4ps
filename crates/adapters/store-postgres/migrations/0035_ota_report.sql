-- Copyright (c) 2026 Pizza 4P's. All rights reserved.
-- Proprietary and confidential. Internal use only. See LICENSE.

-- OTA report: which binary version each store is running and its last self-test outcome
-- ([ADR-0078](../../../../docs/adr/0078-sync-and-ota-closure.md), Track O3). An update report is
-- another kind of liveness contact — the edge telling the cloud "I applied vX and it (did not) pass
-- self-test" — so it extends the O1 `store_liveness` read model rather than opening a new table:
-- the fleet console reads a store's config drift and its OTA progress from one row.
--
--   * `installed_version` — the release the store reported running after its last update cycle, or
--                           NULL if it has never reported (a version string, e.g. `v1.2.3`).
--   * `self_test_ok`      — whether the post-install self-test passed on that version, or NULL.
--   * `reported_at`       — Unix ms of the store's most recent OTA report, or NULL.
--
-- Forward-only and additive, applied idempotently on every boot (ADR-0017): a store that never
-- reports simply leaves these NULL, exactly as the fleet read tolerated before this migration. A
-- report also advances `last_seen_at` (it is contact), so the columns hang off the existing row.
ALTER TABLE store_liveness ADD COLUMN IF NOT EXISTS installed_version text;
ALTER TABLE store_liveness ADD COLUMN IF NOT EXISTS self_test_ok       boolean;
ALTER TABLE store_liveness ADD COLUMN IF NOT EXISTS reported_at        bigint;
