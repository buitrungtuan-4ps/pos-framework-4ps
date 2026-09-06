-- Copyright (c) 2026 Pizza 4P's. All rights reserved.
-- Proprietary and confidential. Internal use only. See LICENSE.
--
-- 0052 — where a store's edge runs (ADR-0110, Program C Phase 1).
--
-- `edge_placement` says which of three machines is the store: a box on the shop LAN, a box the
-- operator owns elsewhere, or one the platform stands up. No line of domain code reads it. What it
-- decides is what the store can promise — ADR-0001's offline guarantee belongs to `IN_STORE` alone —
-- and what a missed heartbeat means: "may well be selling and we cannot see it" for one mode,
-- "dark" for the other two.
--
-- **It lives on `store_lease`, not on `stores`, and that is the decision this file makes.**
--
-- ADR-0110 requires that the value be written inside the lease bump's own transaction and that
-- there be *no other way to write it*. On `stores` that would be a rule somebody has to keep: the
-- table already has a rename-and-archive update path, so the column would sit one careless
-- `UPDATE stores SET …` away from being editable, and an edited column is the one repair ADR-0110
-- names as always wrong — it makes the record agree with a superseded box instead of with the
-- machine that holds authority. On `store_lease` the property is structural: the table's only write
-- is the bump, so the column cannot drift from the generation beside it because the same statement
-- writes both.
--
-- It also reads correctly. `edge_placement` does not mean "where this shop's computer is"; it means
-- **the placement of the machine holding the authoritative generation**. That is a fact about the
-- lease, and it belongs on the lease's row.
--
-- Forward-only and additive, applied idempotently on every boot (ADR-0017).

-- The default is what every store in the fleet already is, so an existing row acquires the truth
-- rather than a placeholder, and a store whose first-ever bump names nothing gets the mode it is
-- actually in. `NOT NULL` because there is no such thing as a store whose edge runs nowhere: the
-- wire's `EDGE_PLACEMENT_UNSPECIFIED` means "this request did not say", which is a property of a
-- request and never of a store.
ALTER TABLE store_lease
    ADD COLUMN IF NOT EXISTS edge_placement text NOT NULL
    DEFAULT 'EDGE_PLACEMENT_IN_STORE';

-- The three modes are a closed framework vocabulary (`EdgePlacement` in `pos-proto`), not tenant
-- data, so the database is the last place to catch a token that never came from it. Unlike
-- `devices.kind` — deliberately left open in 0011 so a new device kind is a data change — a fourth
-- placement is a change to what the framework promises about offline trading, which is an ADR and
-- therefore already a code change; a migration beside it costs nothing.
--
-- Dropped and recreated so re-running the file against a database that already has it is not an
-- error, which is what "applied on every boot" requires. `NOT VALID` is deliberately absent: the
-- rows that exist all carry the default this file just wrote.
ALTER TABLE store_lease DROP CONSTRAINT IF EXISTS store_lease_edge_placement;
ALTER TABLE store_lease ADD CONSTRAINT store_lease_edge_placement
    CHECK (edge_placement IN (
        'EDGE_PLACEMENT_IN_STORE',
        'EDGE_PLACEMENT_HOSTED_BY_OPERATOR',
        'EDGE_PLACEMENT_HOSTED_BY_PLATFORM'
    ));
