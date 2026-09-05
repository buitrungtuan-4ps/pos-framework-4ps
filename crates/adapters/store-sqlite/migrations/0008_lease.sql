-- Copyright (c) 2026 Pizza 4P's. All rights reserved.
-- Proprietary and confidential. Internal use only. See LICENSE.
--
-- 0008 — the lease generation this box holds (ADR-0108, closing ADR-0049's edge half).
--
-- `lease_standing(held, authoritative)` decides whether this machine is still the store. The
-- authoritative generation arrives from the cloud in the `lease` config node. The *held* one is this
-- table, and it exists because of one rule the whole mechanism rests on:
--
--   **A box takes its generation once, on first sight, and thereafter only compares.**
--
-- Without it, supersession is decorative. A box replaced by a new machine pulls the next config, sees
-- generation N+1, adopts it as its own, and calls itself active again — the supersession lasting
-- exactly until the next config pull. So the take is an `INSERT … ON CONFLICT DO NOTHING`, and the
-- rule lives in the schema rather than only in the Rust that happens to call it: the Rust is one
-- refactor away from an `UPDATE`, and this is not.
--
-- It has to be durable for the same reason `0006_ota_state.sql` does: an install deliberately
-- restarts the edge (ADR-0055). A held generation in process memory would be re-adopted from config
-- on every boot, which is the decorative version with extra steps.
--
-- One row per store, and never more than one write to it. There is no `UPDATE` path in this
-- adapter — a box that must legitimately take a newer generation is a box being re-provisioned,
-- which starts from a fresh database.
--
-- Additive-only (ADR-0017): immutable once merged. A change is a new numbered file.

-- `taken_time` is for an operator reading the file — "when did this box decide it was generation 4"
-- — and never for the decision, which is a pure comparison of two numbers with no clock in it
-- (ADR-0049). `generation` is INTEGER because SQLite's INTEGER is 64-bit; the value is written from
-- a u64 and read back through a checked conversion, so a generation past i64::MAX is refused rather
-- than silently wrapped.
CREATE TABLE store_lease (
    store_id   TEXT NOT NULL,
    generation INTEGER NOT NULL,
    taken_time TEXT NOT NULL,
    PRIMARY KEY (store_id)
) WITHOUT ROWID;
