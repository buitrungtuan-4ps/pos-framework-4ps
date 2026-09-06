-- Copyright (c) 2026 Pizza 4P's. All rights reserved.
-- Proprietary and confidential. Internal use only. See LICENSE.
--
-- 0056 — a printer may name the terminal whose transport reaches it (ADR-0112, Program C Phase 1).
--
-- One nullable column, and the nullability is the whole compatibility story: **absent means the edge
-- is the agent**, which is what every store does today and what an in-store placement keeps doing.
-- A fleet takes this release, this column is NULL for every row, and nothing about printing changes.
--
-- It holds another `device_proposals.id` — the same identity space the published node's `device_id`
-- lives in, because a printer's agent is a device the store already knows about. Deliberately **not**
-- a foreign key: the reference has to resolve against the *node being published*, not against the
-- table, and `admin_publish_devices` is where that check belongs because it is the only place the
-- whole set is visible at once. A constraint here would answer a narrower question (does a row
-- exist) than the one that matters (is it a TERMINAL in this store's published set), and would
-- answer it in a way an operator cannot see or correct from the console.
--
-- The `terminal` kind itself needs no schema change: `kind` is `text NOT NULL` with no check
-- constraint, so it carries a third value the way it carried the first two.
--
-- Forward-only and additive, applied idempotently on every boot (ADR-0017).
ALTER TABLE device_proposals ADD COLUMN IF NOT EXISTS agent_device_id text;

-- The publish reads a store's approved devices and resolves every agent reference within that set.
-- Nothing looks a row up *by* its agent, so this index is not for that: it answers "which printers
-- does this terminal serve", which is what the console's terminal detail and a revoke both ask.
CREATE INDEX IF NOT EXISTS device_proposals_agent
    ON device_proposals (tenant_id, agent_device_id) WHERE agent_device_id IS NOT NULL;
