-- Copyright (c) 2026 Pizza 4P's. All rights reserved.
-- Proprietary and confidential. Internal use only. See LICENSE.

-- Reason-code authoring: a tenant's managed list of reasons a void, discount, comp, refund, staff
-- rejection, cash movement or stock correction must cite
-- ([ADR-0115](../../../../docs/adr/0115-reason-codes-are-a-managed-list.md), roadmap B2.2 slice 2).
--
-- `docs/pos-spec.md` §11 item 2 makes reasons from a cloud-managed list one of the six fraud
-- controls, and eleven event fields declare a `reason_code_id` documented as coming from it. Nothing
-- produced that list: `ReasonCodeId` appeared only in two port signatures and in test fixtures. This
-- is where the entries an operator authors live between edits, before a publish assembles them into
-- the `reason_codes` config node.
--
-- Authored per TENANT, not per store. "Waste" and "staff error" mean the same thing in every store a
-- brand runs, and a per-store list would make the per-employee void-rate comparison §11 item 3 asks
-- for meaningless across stores.
--
--   * `entity_id` — the reason code's own id, a ULID string, so ordering by it is creation order for
--                   a stable diff.
--   * `doc`       — the whole authored record (the wire `PublishedReasonCode`, including `active` and
--                   `applies_to`) held as `jsonb`, the same store-the-shape-as-a-document choice
--                   `inventory_items` (0037) and `campaigns` (0032) make. `pos-cloud` does the
--                   (de)serialisation; no cloud-domain type leaks into the adapter.
--
-- One table rather than a `kind` discriminator like 0037, because there is one entity here and a
-- discriminator with a single value is a column that only ever lies about being useful.
--
-- WHY THERE IS NO CASCADE, AND WHY DELETE IS NOT THE RETIREMENT PATH
--
-- A historic `sales.order_line.voided` names a `reason_code_id` forever; the event log is immutable
-- and additive-only. Deleting the row makes that event unresolvable, and a fraud control whose audit
-- trail decays into unreadable ULIDs has stopped being one. So the way an operator takes a reason out
-- of service is `doc->>'active' = false`, which keeps the row resolvable; DELETE exists for the entry
-- created by mistake and never cited. Nothing here references another table, so nothing cascades into
-- it either.
--
-- A reason code is a code, a name and the actions it is valid for — T2 (Confidential) configuration
-- and reference data. The row carries no customer or employee identifier: who cited a reason lives on
-- the event, not here.
--
-- Tenant-scoped exactly like the rest of the config data (0012/0028/0032/0037): RLS on
-- `app.tenant_id`, a grant to `app_tenant` (CRUD is per-record, so INSERT/UPDATE/DELETE as well as
-- SELECT), the trusted pool owner bypassing RLS. Forward-only and additive, applied idempotently on
-- every boot (ADR-0017). Greenfield — no backfill; a tenant with no rows publishes no node and the
-- edge keeps `PublishedReasonCodes::framework_default()`, which is ADR-0115's whole point.

CREATE TABLE IF NOT EXISTS reason_codes (
    tenant_id  text        NOT NULL,
    entity_id  text        NOT NULL,
    doc        jsonb       NOT NULL,
    updated_at timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (tenant_id, entity_id)
);

ALTER TABLE reason_codes ENABLE ROW LEVEL SECURITY;
DROP POLICY IF EXISTS reason_codes_tenant_isolation ON reason_codes;
CREATE POLICY reason_codes_tenant_isolation ON reason_codes
    USING (tenant_id = current_setting('app.tenant_id', true));
GRANT SELECT, INSERT, UPDATE, DELETE ON reason_codes TO app_tenant;
