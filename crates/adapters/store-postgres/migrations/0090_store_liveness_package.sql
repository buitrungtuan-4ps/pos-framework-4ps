-- Copyright (c) 2026 Pizza 4P's. All rights reserved.
-- Proprietary and confidential. Internal use only. See LICENSE.

-- What a store's heartbeat says about its package
-- ([ADR-0189](../../../../docs/adr/0189-a-store-runs-a-package-version-hq-pins.md) decision 6), on
-- its liveness row, so the console can show the version a store runs and a configuration it refused
-- for its package.
--
-- `package_version_id` is the version the store runs, NULL for its release's default package.
-- `package_refused_config_version_id`, `package_refused_version_id` and `package_refusal_reason` name
-- the configuration it last refused, while the latest one stands refused; NULL otherwise.
-- `package_reported_at` is when a heartbeat last said anything about the package, NULL if none ever
-- has: an older edge. A beat that reports replaces all four, so a refusal that has cleared clears.
--
-- Forward-only and additive, applied idempotently on every boot (ADR-0017). No backfill: a store
-- reports on its next heartbeat.

ALTER TABLE store_liveness ADD COLUMN IF NOT EXISTS package_version_id text;
ALTER TABLE store_liveness ADD COLUMN IF NOT EXISTS package_refused_config_version_id text;
ALTER TABLE store_liveness ADD COLUMN IF NOT EXISTS package_refused_version_id text;
ALTER TABLE store_liveness ADD COLUMN IF NOT EXISTS package_refusal_reason text;
ALTER TABLE store_liveness ADD COLUMN IF NOT EXISTS package_reported_at bigint;
