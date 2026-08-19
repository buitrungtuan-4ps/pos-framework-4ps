-- Copyright (c) 2026 Pizza 4P's. All rights reserved.
-- Proprietary and confidential. Internal use only. See LICENSE.
--
-- 0001 — the event store, outbox, and config snapshot (ADR-0015, ADR-0026).
--
-- This is the schema `store-sqlite` writes as an EventStore and ConfigStore. The domain
-- projection tables (orders, bills, shifts, stock ledger, the receipt counter, the flat
-- read tables) are a later, additive migration written by the edge application layer; this
-- migration is only the append-only log and the config the store syncs.
--
-- Additive-only (ADR-0017): this file is immutable once merged. A schema change is a new
-- numbered file, never an edit here.

-- The append-only event log. One row per (store, event); the primary key is the idempotency
-- key, so a replayed append is an INSERT OR IGNORE that keeps the stored copy (ADR-0026 §5).
-- WITHOUT ROWID: the composite key IS the storage order, and reading a store's events ordered
-- by event_id is a range scan on that key — the ordered-read-back contract, for free.
CREATE TABLE events (
    store_id TEXT NOT NULL,
    event_id TEXT NOT NULL,
    envelope TEXT NOT NULL,
    PRIMARY KEY (store_id, event_id)
) WITHOUT ROWID;

-- The durable outbox. `position` is an AUTOINCREMENT rowid, so it is assigned at insert time
-- — which is commit time, because rows are only inserted inside the commit transaction — and
-- is monotone and never reused even after acknowledged rows are deleted (ADR-0026 §3). It
-- starts at one, so it never collides with OutboxPosition::START (zero, "before every event").
CREATE TABLE outbox (
    position INTEGER PRIMARY KEY AUTOINCREMENT,
    store_id TEXT NOT NULL,
    envelope TEXT NOT NULL
);

CREATE INDEX idx_outbox_store_id_position ON outbox (store_id, position);

-- The config version the store is currently running, and the last version that applied
-- cleanly. They diverge only after a refused delta: a bad version never commits, so `current`
-- stays equal to `last_known_good` (ConfigStore contract, ADR-0026).
CREATE TABLE config_current (
    store_id TEXT PRIMARY KEY,
    snapshot TEXT NOT NULL
);

CREATE TABLE config_last_known_good (
    store_id TEXT PRIMARY KEY,
    snapshot TEXT NOT NULL
);
