-- Copyright (c) 2026 Pizza 4P's. All rights reserved.
-- Proprietary and confidential. Internal use only. See LICENSE.
--
-- 0014 — an index over the events that carry no chain (ADR-0131, finding F5).
--
-- Additive-only (ADR-0017): this file is immutable once merged.
--
-- Finding the chain anchor and walking the chain both count this store's unchained rows —
-- `COUNT(*) … WHERE store_id = ?1 AND seq IS NULL`. With no index for that predicate the count
-- read every row of `events`, and because `seq` was added after `envelope`, on a row that spills
-- to an overflow page the column the query needs sits on the overflow page: a full read of the
-- log to learn, on every store opened after 0013, that the answer is zero.
--
-- Partial on `seq IS NULL`, so it holds only the rows written before the chain existed. A store
-- installed after 0013 has none, and pays nothing for it.
CREATE INDEX idx_events_store_id_unchained
    ON events (store_id)
    WHERE seq IS NULL;
