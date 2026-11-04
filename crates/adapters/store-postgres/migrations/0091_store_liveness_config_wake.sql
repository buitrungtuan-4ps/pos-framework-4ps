-- Copyright (c) 2026 Pizza 4P's. All rights reserved.
-- Proprietary and confidential. Internal use only. See LICENSE.

-- What a store's heartbeat says about how configuration reaches it
-- ([ADR-0195](../../../../docs/adr/0195-the-cloud-wakes-a-store-to-pull-its-config.md)), on its
-- liveness row, so the console can show which stores run on the poll alone and how long a published
-- change takes to apply on each.
--
-- `config_delivery` is `CONFIG_DELIVERY_WAKE` or `CONFIG_DELIVERY_POLL`. `config_wake_samples` is how
-- many wake-to-apply times the store drew `config_wake_p95_ms` and `config_wake_max_ms` from, NULL
-- with none. `config_wake_reported_at` is when a heartbeat last said anything about it, NULL if none
-- ever has: an older edge. A beat that reports replaces all five.
--
-- Forward-only and additive, applied idempotently on every boot (ADR-0017). No backfill: a store
-- reports on its next heartbeat.

ALTER TABLE store_liveness ADD COLUMN IF NOT EXISTS config_delivery text;
ALTER TABLE store_liveness ADD COLUMN IF NOT EXISTS config_wake_samples integer;
ALTER TABLE store_liveness ADD COLUMN IF NOT EXISTS config_wake_p95_ms bigint;
ALTER TABLE store_liveness ADD COLUMN IF NOT EXISTS config_wake_max_ms bigint;
ALTER TABLE store_liveness ADD COLUMN IF NOT EXISTS config_wake_reported_at bigint;
