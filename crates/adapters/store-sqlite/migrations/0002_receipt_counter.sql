-- Copyright (c) 2026 Pizza 4P's. All rights reserved.
-- Proprietary and confidential. Internal use only. See LICENSE.
--
-- 0002 — the store's own receipt-number counter (ADR-0025, the `store_server` authority).
--
-- The store server owns one counter per store and hands out the next number. Because every
-- allocation goes through the one writer thread and one SQLite file (ADR-0015), two cashier
-- devices cannot collide: the sequence is gapless while this single authority is reachable,
-- which is the honest guarantee ADR-0025 records. This is the store's receipt number, NOT a
-- legal invoice number — that is the country module's, from a pre-allocated range, and the
-- two must never be conflated.
--
-- Additive-only (ADR-0017): immutable once merged. A change is a new numbered file.

-- One counter per store. `next_number` is the number the next allocation will hand out; it
-- starts at 1, so the first receipt is 1.
CREATE TABLE receipt_counter (
    store_id    TEXT PRIMARY KEY,
    next_number INTEGER NOT NULL
);

-- Which number a bill was given. This is what makes allocation idempotent: a retry for a bill
-- that already has a number returns the same one and does not advance the counter, so a crash
-- between allocating and appending the settle event cannot skip a number. The composite key IS
-- the lookup, so a WITHOUT ROWID table is both the storage and the index.
CREATE TABLE receipt_allocations (
    store_id       TEXT NOT NULL,
    bill_id        TEXT NOT NULL,
    receipt_number INTEGER NOT NULL,
    PRIMARY KEY (store_id, bill_id)
) WITHOUT ROWID;
