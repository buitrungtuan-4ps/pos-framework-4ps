-- Copyright (c) 2026 Pizza 4P's. All rights reserved.
-- Proprietary and confidential. Internal use only. See LICENSE.
--
-- 0003 — the daily takeaway queue-number counter (ADR-0064, the edge `OrderIn` authority).
--
-- A tableless order (a marketplace order, the public API, a takeaway guest) has no table to be
-- called back to, so the store hands it a queue number the counter shouts out. Unlike the receipt
-- number (0002), which is gapless-forever per store, the queue number RESETS each business date:
-- the counter is keyed by (store, business_date), so the first order of a new trading day is #1
-- again with no cron and no midnight job — a business date the counter has never seen simply has
-- no row yet and starts at 1. Every allocation goes through the one writer thread and one SQLite
-- file (ADR-0015), so two channels delivering at once cannot be handed the same number.
--
-- Additive-only (ADR-0017): immutable once merged. A change is a new numbered file.

-- One counter per (store, business_date). `next_number` is the number the next allocation will
-- hand out; it starts at 1, so the first takeaway of the day is 1. A new business date has no row,
-- which is exactly the daily reset. The composite key IS the lookup, so WITHOUT ROWID is both the
-- storage and the index.
CREATE TABLE queue_counter (
    store_id      TEXT NOT NULL,
    business_date TEXT NOT NULL,
    next_number   INTEGER NOT NULL,
    PRIMARY KEY (store_id, business_date)
) WITHOUT ROWID;

-- Which number an order was given. This is what makes allocation idempotent: a retry for an order
-- that already has a number returns the same one and does not advance the counter, so a crash
-- between allocating and recording the acceptance cannot burn two numbers on one order or shout the
-- same number at two customers. Keyed by order, not by (store, date), because an order id already
-- names its store and the acceptance is what the caller retries on.
CREATE TABLE queue_allocations (
    store_id     TEXT NOT NULL,
    order_id     TEXT NOT NULL,
    queue_number INTEGER NOT NULL,
    PRIMARY KEY (store_id, order_id)
) WITHOUT ROWID;
