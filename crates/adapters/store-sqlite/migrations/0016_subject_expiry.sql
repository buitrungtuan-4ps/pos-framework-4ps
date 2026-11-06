-- Copyright (c) 2026 Pizza 4P's. All rights reserved.
-- Proprietary and confidential. Internal use only. See LICENSE.
--
-- 0016 — a subject record may carry its own expiry (ADR-0206 decision 1).
--
-- Additive-only (ADR-0017): this file is immutable once merged.
--
-- A personal custom value is kept for the days its field declares, not for the store's default, so
-- its record says when it expires. The sweep masks an unmasked row whose `retain_until` is at or
-- before now; a row without one, which is every row written before this file and every B2B buyer's
-- record (ADR-0107), keeps the old rule and the `subjects_due` index. The new index is what the
-- sweep's expiry branch runs on: unmasked rows with an expiry, for one store, soonest first.
ALTER TABLE subjects ADD COLUMN retain_until INTEGER;

CREATE INDEX subjects_expiry ON subjects (store_id, retain_until)
    WHERE masked_at IS NULL AND retain_until IS NOT NULL;
