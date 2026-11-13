-- Copyright (c) 2026 Pizza 4P's. All rights reserved.
-- Proprietary and confidential. Internal use only. See LICENSE.
--
-- 0101 — a cloud subject names its store and may carry its own expiry
-- ([ADR-0215](../../../../docs/adr/0215-a-delivery-order-names-its-vendor.md) decision 3,
-- [ADR-0206](../../../../docs/adr/0206-a-personal-custom-value-stays-on-its-store-and-expires-on-its-own-clock.md)
-- decision 1).
--
-- A delivery order's recipient (name, phone, address) is kept here, apart from the order relay's
-- queue, under the store's retention. Two columns, both nullable, so every existing row reads as
-- before:
--
--   * `store_id`     — the store the record was collected for. Only that store's credential may
--                      read it (`GET /sync/stores/{store_id}/subjects/{subject_id}`); a row
--                      without one is read by no store.
--   * `retain_until` — when the record is masked, in Unix milliseconds: the store's own retention
--                      (its locale's `default_retention_days`) counted from `collected_at`. A row
--                      without one keeps ADR-0035's rule, masked once `collected_at` is past the
--                      cloud's `retention_days`.
--
-- RETENTION: unchanged in kind. The sweep masks a row once its `retain_until` is at or before now,
-- or, without one, once it is past the cloud's period. Personal data: `fields` only, as before.
-- Forward-only and additive, applied idempotently on every boot (ADR-0017).

ALTER TABLE subjects ADD COLUMN IF NOT EXISTS store_id text;
ALTER TABLE subjects ADD COLUMN IF NOT EXISTS retain_until bigint;

-- Answers the sweep's "unmasked and past its own expiry" query without a scan.
CREATE INDEX IF NOT EXISTS subjects_unmasked_by_retain_until
    ON subjects (retain_until) WHERE masked_at IS NULL AND retain_until IS NOT NULL;
