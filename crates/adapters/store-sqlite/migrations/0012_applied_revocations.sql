-- Copyright (c) 2026 Pizza 4P's. All rights reserved.
-- Proprietary and confidential. Internal use only. See LICENSE.
--
-- 0012 — the revocations this store has already carried out for the cloud (ADR-0118 §6).
--
-- The console can retire a till on a store nobody can reach, by publishing the device's local id
-- onto a `revoked_devices` deny-list on the config rail. That list only ever grows, and it rides
-- *every* config pull. Without a record of what has already been done, the store would revoke every
-- id on the list every thirty seconds forever: each pass a durable write and a fresh
-- `device.admission.revoked` into the outbox, so one lost tablet would become an unbounded stream of
-- duplicate events for the cloud to fold.
--
-- Its own table, and not a column on `paired_devices`, for a reason the pairing table makes
-- unavoidable: revoking a device DELETEs its row (0005), and `record_pairing` deliberately
-- overwrites the row on a re-pair (0011). A flag there would be destroyed by the very act it
-- records, and the loop above would run anyway.
--
-- One row per device id, insert-only. There is deliberately no delete path and no adapter method
-- that removes a row: the whole value of the record is that it cannot be taken back. A tablet that
-- legitimately comes back is a *fresh pairing* with a fresh id (ADR-0030 mints one per pairing),
-- which this table never names.
--
-- Note what this table is NOT. It is not what makes a revocation durable — the `paired_devices`
-- DELETE is, and `Pairing::load()` cannot resurrect a row that is gone, so a config rollback that
-- shortens the deny-list cannot hand a retired tablet its access back whether or not this table
-- exists. This is only what makes the directive one-shot.
--
-- No `store_id`: the edge database is one store's, exactly as `paired_devices` and `device_sessions`
-- are unkeyed by store. The store-keyed tables beside it (`ota_self_test`, `store_lease`,
-- `intake_ledger`, `subjects`) carry one because their rows are *about* a store; these rows are
-- about a device.
--
-- Additive-only (ADR-0017): immutable once merged. A change is a new numbered file.

-- `device_id` is the local, edge-minted device ULID as text — the same identity space as
-- `paired_devices.device_id`, and deliberately NOT the console's own device id (nothing joins those
-- two, by decision). `applied_time` is for an operator reading the file; nothing decides on it, and
-- the deny-list subtraction is a pure set difference on `device_id`.
--
-- No foreign key to `paired_devices`: the row has to outlive the pairing, which is the point.
CREATE TABLE applied_device_revocations (
    device_id    TEXT NOT NULL,
    applied_time TEXT NOT NULL,
    PRIMARY KEY (device_id)
) WITHOUT ROWID;
