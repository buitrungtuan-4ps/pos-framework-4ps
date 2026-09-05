-- Copyright (c) 2026 Pizza 4P's. All rights reserved.
-- Proprietary and confidential. Internal use only. See LICENSE.

-- The store's own publish backlog on the fleet read model (production-readiness **O6**).
--
-- `EventStore::outbox_depth` counts the events a store has committed and not yet published to the
-- cloud. It is implemented in both adapters and covered by the contract suite, and until this column
-- existed it had **no production caller at all** — so a store quietly falling behind on its event
-- publish was invisible to everyone, including the operator whose reports were going stale because
-- of it.
--
-- Distinct from `relay_backlog`, which the fleet read already carries: that counts orders queued
-- *for* a store's till, this counts events queued *from* it. Opposite directions, and a store can be
-- healthy on one and badly behind on the other.
--
--   * `outbox_depth`       — the count the store last reported, or NULL if it never has (an older
--                            edge, or one that has not beaten since this shipped). NULL is not zero:
--                            "no backlog" and "never told us" are different answers and the console
--                            renders them differently.
--   * `outbox_reported_at` — Unix ms of that report, so a stale figure is visibly stale rather than
--                            silently trusted. A store can be reachable while its last depth reading
--                            is hours old, and the two facts must not be conflated.
--
-- Forward-only and additive, applied idempotently on every boot (ADR-0017). Both columns are
-- nullable, so the migration is a no-op for a store that has never reported and the row keeps
-- whatever liveness it already had.
ALTER TABLE store_liveness ADD COLUMN IF NOT EXISTS outbox_depth bigint;
ALTER TABLE store_liveness ADD COLUMN IF NOT EXISTS outbox_reported_at bigint;
