-- Copyright (c) 2026 Pizza 4P's. All rights reserved.
-- Proprietary and confidential. Internal use only. See LICENSE.
--
-- 0010 — which paired device holds which terminal's print-agent identity (ADR-0112).
--
-- Two identity spaces meet in this table, and that is its whole reason for existing. The
-- `agent_device_id` is a **cloud-approved** device id: the `TERMINAL` entry a console admin created,
-- which arrives in the published `devices` node. The `paired_device_id` is a **locally minted**
-- pairing id (ADR-0030, `pairing.rs`: "unique per pairing; the cloud's approved-device registry is a
-- separate identity this local id does not claim to be"). Nothing else in the tree joins them, and a
-- printer's `agent_device_id` is useless to the edge until something does.
--
-- **Why durable.** The binding is a managerial act performed once, in front of the machine, behind a
-- manager's PIN. Holding it in memory would mean re-doing it after every edge restart — at the box,
-- with a manager present, in the middle of service. `pairing.rs` records a device before it answers
-- for the same reason, and this table follows it.
--
-- **Exclusive in both directions, and both are enforced here rather than by a handler that checks
-- first.** A primary key on `agent_device_id`: one terminal identity is held by at most one device,
-- because two devices holding one identity both claim from the same queue and each ticket prints
-- once — on whichever box grabbed it. Half the kitchen's tickets end up in an apron pocket and
-- nobody finds out until service; refusing is visible, splitting is not. A unique index on
-- `paired_device_id`: one device holds at most one terminal identity, because a terminal *is* a
-- machine and so is a paired device, and a box that answered for two would be inventing a machine
-- that is not in the shop.
--
-- **`last_seen_at` is written by the agent's own claims, not by a heartbeat of its own.** The one
-- question that matters — has this agent been heard from — is answered by the act that proves it:
-- asking for work. A separate liveness ping would be a second thing that can be true while printing
-- is broken.
--
-- Additive-only, applied idempotently on every boot (ADR-0017). Holds no personal data: two opaque
-- ids and two instants, so unlike `print_jobs` beside it this table carries nothing to redact.
CREATE TABLE IF NOT EXISTS print_agents (
    agent_device_id  TEXT    NOT NULL,
    paired_device_id TEXT    NOT NULL,
    -- Unix milliseconds, as every instant in this database is.
    bound_at         INTEGER NOT NULL,
    -- When this agent last asked for work. The enqueue reads it to decide whether a queue should
    -- start building behind this box at all.
    last_seen_at     INTEGER NOT NULL,
    PRIMARY KEY (agent_device_id)
) WITHOUT ROWID;

-- The other direction of the exclusivity, and the lookup every agent route makes first: the request
-- arrives carrying a paired device, and the route has to turn that into the agent it speaks for.
CREATE UNIQUE INDEX IF NOT EXISTS print_agents_device ON print_agents (paired_device_id);
