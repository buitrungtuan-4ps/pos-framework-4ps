-- Copyright (c) 2026 Pizza 4P's. All rights reserved.
-- Proprietary and confidential. Internal use only. See LICENSE.
--
-- 0067 — the anchors a store publishes, and the contradictions among them
-- ([ADR-0131](../../../docs/adr/0131-a-chained-event-log.md) decision 4).
--
-- A hash chain alone catches careless editing and storage corruption, not fraud: the hash function
-- is in the source, so anyone holding a store's database file can rewrite a record, re-derive every
-- later link, and produce a chain that verifies. `store-sqlite/tests/chain.rs` has two tests that
-- assert exactly that, so nobody believes otherwise.
--
-- What a store cannot do is rewrite what the cloud already received. These two tables are that
-- record. `chain_anchors` holds every head a store published; `chain_anchor_conflicts` holds every
-- offered head the cloud refused because it contradicted one it already had.
--
-- **The primary key is the whole mechanism.** `(tenant_id, store_id, chain_seq)` means a store gets
-- exactly one head per chain length, for ever. A second, different head at a length already
-- recorded is not an update to apply — it is two irreconcilable claims about one log, and the row
-- that is already here is the evidence. So the write is `ON CONFLICT DO NOTHING`: an identical
-- re-delivery (at-least-once ingest, ADR-0026 §4; reconciliation's re-push, ADR-0040) lands on the
-- same row and is a no-op, and a contradicting one cannot overwrite it even if the application
-- layer above were to ask.
--
-- **Two timestamps, and only one of them is trustworthy.** `observed_at` is the store's own clock on
-- the anchor event — its claim about when the shift closed — and a store that is tampering with its
-- log controls it. `noticed_at` on a conflict is the server's `now()`, which is a fact about the
-- cloud, and it is the one an investigation can rely on.
--
-- No PII: store and tenant identifiers, counts, hashes and an event id. A chain hash is a digest of
-- an envelope whose every payload field `pos_proto::pii` has already proven free of personal data
-- (ADR-0131), not of a customer record.
--
-- Tenant-scoped like the rest: RLS on `app.tenant_id`, a grant to `app_tenant`, the trusted ingest
-- pool owner bypassing RLS. Forward-only and additive, applied idempotently on every boot (ADR-0017).

CREATE TABLE IF NOT EXISTS chain_anchors (
    tenant_id   text   NOT NULL,
    store_id    text   NOT NULL,
    -- How many records the store's chain held when the anchor was taken.
    chain_seq   bigint NOT NULL,
    -- The hash of the last of them, 64 lowercase hex characters.
    chain_head  text   NOT NULL,
    -- How many records carry no chain because they predate it. Not a fault: it tells a store that
    -- upgraded mid-life from one that lost its history.
    unchained   bigint NOT NULL,
    -- The store's own timestamp on the anchor event, in Unix milliseconds. Its claim, not a finding.
    observed_at bigint NOT NULL,
    -- When the cloud filed it. The server's clock, for the same reason `noticed_at` below is.
    recorded_at timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (tenant_id, store_id, chain_seq)
);

-- Answers "what is this store's head?" — an index-only backwards scan to the first row.
CREATE INDEX IF NOT EXISTS chain_anchors_head
    ON chain_anchors (tenant_id, store_id, chain_seq DESC);

ALTER TABLE chain_anchors ENABLE ROW LEVEL SECURITY;
DROP POLICY IF EXISTS chain_anchors_tenant_isolation ON chain_anchors;
CREATE POLICY chain_anchors_tenant_isolation ON chain_anchors
    USING (tenant_id = current_setting('app.tenant_id', true));
-- No UPDATE and no DELETE, to anyone. A grant that allowed either would hand back exactly the power
-- this table exists to take away.
GRANT SELECT, INSERT ON chain_anchors TO app_tenant;

CREATE TABLE IF NOT EXISTS chain_anchor_conflicts (
    conflict_id      text   PRIMARY KEY,
    tenant_id        text   NOT NULL,
    store_id         text   NOT NULL,
    -- The chain length both heads claim to describe.
    chain_seq        bigint NOT NULL,
    -- What the cloud held at that length, and what it was offered instead. Both are kept: the
    -- finding *is* the pair, and either one alone says nothing.
    held_head        text   NOT NULL,
    offered_head     text   NOT NULL,
    -- The anchor event that carried the offered head, so the finding traces back to a record.
    offered_event_id text   NOT NULL,
    -- The cloud's clock. See the note above on which of the two timestamps can be relied on.
    noticed_at       timestamptz NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS chain_anchor_conflicts_by_store
    ON chain_anchor_conflicts (tenant_id, store_id, noticed_at DESC);
-- The tenant-wide read the console's list uses, with no store filter.
CREATE INDEX IF NOT EXISTS chain_anchor_conflicts_by_tenant
    ON chain_anchor_conflicts (tenant_id, noticed_at DESC);

ALTER TABLE chain_anchor_conflicts ENABLE ROW LEVEL SECURITY;
DROP POLICY IF EXISTS chain_anchor_conflicts_tenant_isolation ON chain_anchor_conflicts;
CREATE POLICY chain_anchor_conflicts_tenant_isolation ON chain_anchor_conflicts
    USING (tenant_id = current_setting('app.tenant_id', true));
GRANT SELECT, INSERT ON chain_anchor_conflicts TO app_tenant;
