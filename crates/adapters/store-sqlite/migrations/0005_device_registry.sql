-- Copyright (c) 2026 Pizza 4P's. All rights reserved.
-- Proprietary and confidential. Internal use only. See LICENSE.
--
-- 0005 — durable device pairing and sign-in (ADR-0091, roadmap v3 slice S0d).
--
-- Both tables used to be process memory, so a power blip, an OTA install (ADR-0055 restarts the
-- edge on purpose) or a `systemctl restart` unpaired every tablet in the store at once, mid-service.
-- These two tables are what make them survive.
--
-- THE TOKEN IS NOT STORED. `token_digest` is a SHA-256 of the device token, hex-encoded, computed
-- by the edge before it ever reaches this adapter. A bearer token written here in the clear would
-- mean a stolen till, a copied disk image, or a backup that left the store hands over working
-- credentials for every device — and a SQLite file is far easier to walk off with than a running
-- process's heap, which is exactly what changes when this becomes durable. A digest cannot be
-- presented: the gate hashes what the client sent and compares digests.
--
-- No PIN and no PIN hash appear here either. `device_sessions` holds identifiers and two instants.
--
-- `last_seen_at_ms` is what the idle timeout reads. The timeout itself is NOT enforced here or
-- anywhere in this adapter: the edge decides expiry, so the rule is one pure comparison tested
-- without a database and every adapter behaves identically because none of them knows it.
--
-- ON DELETE CASCADE is the invariant that matters: revoking a device must not leave a session
-- behind, because a sign-in belonging to no paired device is unreachable state a later feature
-- could read as live. It is enforced by the schema rather than by two statements the caller has to
-- remember — note that SQLite honours it only with `PRAGMA foreign_keys = ON`, which this adapter
-- sets on every connection, so the adapter also deletes the session explicitly. Belt and braces on
-- purpose: the schema documents the intent, the code guarantees it.
--
-- Additive-only (ADR-0017): immutable once merged. A change is a new numbered file.

CREATE TABLE paired_devices (
    device_id    TEXT NOT NULL PRIMARY KEY,
    token_digest TEXT NOT NULL UNIQUE,
    paired_at_ms INTEGER NOT NULL
) WITHOUT ROWID;

-- The gate's lookup on every request: digest in, device out. UNIQUE above already indexes it; this
-- name makes the intent explicit to anyone reading the schema.
CREATE INDEX paired_devices_by_digest ON paired_devices (token_digest);

CREATE TABLE device_sessions (
    device_id       TEXT NOT NULL PRIMARY KEY
                    REFERENCES paired_devices (device_id) ON DELETE CASCADE,
    employee_id     TEXT NOT NULL,
    signed_in_at_ms INTEGER NOT NULL,
    last_seen_at_ms INTEGER NOT NULL
) WITHOUT ROWID;
