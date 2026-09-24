-- Copyright (c) 2026 Pizza 4P's. All rights reserved.
-- Proprietary and confidential. Internal use only. See LICENSE.
--
-- 0015 — where the kept log begins (ADR-0145).
--
-- Additive-only (ADR-0017): this file is immutable once merged.
--
-- The edge deletes an event once it is synced, older than the store's retention and needed by
-- nothing still open, always as a prefix in `seq` order. This row records the `seq` and `hash` of
-- the last record deleted, written in the same transaction as the delete, so the chain walk starts
-- here rather than at genesis and still checks every link of what is kept (ADR-0131). No row means
-- nothing has been deleted, and the walk starts at genesis as before.
CREATE TABLE chain_checkpoint (
    store_id TEXT    NOT NULL PRIMARY KEY,
    seq      INTEGER NOT NULL,
    hash     TEXT    NOT NULL
) WITHOUT ROWID;
