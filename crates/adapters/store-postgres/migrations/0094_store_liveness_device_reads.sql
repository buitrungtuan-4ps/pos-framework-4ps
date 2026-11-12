-- Copyright (c) 2026 Pizza 4P's. All rights reserved.
-- Proprietary and confidential. Internal use only. See LICENSE.

-- Which package version each of a store's paired tills last read, as its heartbeat says
-- (roadmap-v3 B7.3, per-device drift), on its liveness row, so the console can show a till that
-- missed a `config_applied` and still draws an older version than its store runs.
--
-- `device_reads` is a JSON array of `{device_id, package_version_id, read_at_ms}`, at most 64, which
-- the edge bounds and the cloud checks before writing. `device_reads_reported_at` is when a heartbeat
-- last reported them, NULL if none ever has: an older edge. A beat that reports replaces both. A
-- device id names a device, never a person.
--
-- Forward-only and additive, applied idempotently on every boot (ADR-0017). No backfill: a store
-- reports on its next heartbeat.

ALTER TABLE store_liveness ADD COLUMN IF NOT EXISTS device_reads jsonb;
ALTER TABLE store_liveness ADD COLUMN IF NOT EXISTS device_reads_reported_at bigint;
