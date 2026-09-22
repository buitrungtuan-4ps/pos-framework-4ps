-- Copyright (c) 2026 Pizza 4P's. All rights reserved.
-- Proprietary and confidential. Internal use only. See LICENSE.
--
-- 0013 — the hash chain over the event log (ADR-0131).
--
-- `events` was append-only by convention and by nothing else: three columns with no link
-- between rows, in a SQLite file on a PC in a shop. An UPDATE to an amount or a DELETE of a
-- settled bill left `PRAGMA integrity_check` reporting `ok`, because no constraint had been
-- broken and nothing recorded what had been there.
--
-- Additive-only (ADR-0017): this file is immutable once merged.
--
-- All three columns are NULLABLE, and that is the decision rather than an omission. A row
-- written before this migration has no chain and never will: backfilling would compute a
-- chain over history nobody can vouch for, which is exactly the false confidence ADR-0131
-- rejects a bare chain for. Those rows verify as **unchained**, never as broken.
ALTER TABLE events ADD COLUMN seq INTEGER;
ALTER TABLE events ADD COLUMN prev_hash TEXT;
ALTER TABLE events ADD COLUMN hash TEXT;

-- Finding the chain head is the one query on the append path, once per commit: the highest
-- `seq` for this store. Partial on `seq IS NOT NULL` so the index holds only chained rows —
-- a store upgrading with a long unchained history pays nothing for it.
CREATE INDEX idx_events_store_id_seq
    ON events (store_id, seq)
    WHERE seq IS NOT NULL;
