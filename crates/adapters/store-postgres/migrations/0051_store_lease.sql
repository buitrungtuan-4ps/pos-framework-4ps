-- Copyright (c) 2026 Pizza 4P's. All rights reserved.
-- Proprietary and confidential. Internal use only. See LICENSE.
--
-- 0051 — the store's authoritative lease generation, and the one each box reports holding
-- (ADR-0108, closing ADR-0049's cloud half).
--
-- ADR-0049 made "one store, one active machine" a comparison of two `LeaseGeneration`s and deferred
-- *persisting the authoritative one* to `Fiscalization` in P10, bundled with allocating a legal
-- invoice range. The bundle is why neither was built: the range needs a tax authority, which is a
-- per-country registration question; the generation needs a row and an increment. ADR-0108 splits
-- them, and this is the row.
--
-- Two tables' worth of change, both about the lease and deliberately in one file:
--
--   * `store_lease` is the **authority**. One row per store; the only write is a bump. It is not a
--     config node, because a config node is authored (a typo would promote a machine) and a config
--     tree rolls back (ADR-0094), and a generation a rollback can move backwards is not monotonic.
--     The `lease` node the edge reads is *derived* from this row by the bump that wrote it.
--
--   * `store_liveness` gains what each box says it holds. Without it a split is invisible: a store
--     that has been replaced and a store that has simply not pulled config yet look identical from
--     the console. NULL is "did not say", never "generation 0" — 0 is a real first generation, so
--     conflating them would report a box as current when it has told us nothing.
--
-- Forward-only and additive, applied idempotently on every boot (ADR-0017).

-- The authority. `generation` starts at 0 for a store's first lease (ADR-0049: "the first lease a
-- store ever issues is generation 0") and only ever increases; the adapter's bump is
-- `generation + 1` with no setter beside it, because an authority that takes a number from its
-- caller is not one. `issued_at` is for an operator reading the trail, never for the decision, which
-- is a pure comparison with no clock in it.
CREATE TABLE IF NOT EXISTS store_lease (
    tenant_id  text   NOT NULL,
    store_id   text   NOT NULL,
    generation bigint NOT NULL,
    issued_at  bigint NOT NULL,
    PRIMARY KEY (tenant_id, store_id)
);

-- What the box last reported holding, and when it said so — the second half of the split, and the
-- reason a stale figure reads as stale rather than as current.
ALTER TABLE store_liveness ADD COLUMN IF NOT EXISTS lease_generation bigint;
ALTER TABLE store_liveness ADD COLUMN IF NOT EXISTS lease_reported_at bigint;
