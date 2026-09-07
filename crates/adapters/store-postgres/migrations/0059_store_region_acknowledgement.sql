-- Copyright (c) 2026 Pizza 4P's. All rights reserved.
-- Proprietary and confidential. Internal use only. See LICENSE.
--
-- 0059 — a person's answer to the region warning (ADR-0114, Program C Phase 1).
--
-- 0058 recorded *where* a hosted store's machine is, and the console now compares that against the
-- country the store publishes. A store whose data rests in a country other than its own draws a
-- warning that **never blocks** — whether a particular cross-border placement is lawful depends on
-- the basis for the transfer and what was said when consent was taken, which are facts the operator
-- holds under law that changes without a release. So the warning has to be answerable, and this
-- table is the answer: one row saying which two countries were being compared, why the difference
-- is correct, and when somebody said so.
--
-- **The row records the comparison, not just the verdict.** `profile_country` and `region_country`
-- are stored beside the reason rather than derived at read time, and that is the whole mechanism: a
-- reason written about SG-versus-VN says nothing about the store once it moves to JP. The read
-- compares the store's *current* pair against the pair on this row, and an acknowledgement that no
-- longer matches simply stops counting — the warning comes back, unanswered, which is correct
-- because nobody has answered *this* difference. Nothing clears the row and nothing needs to: it is
-- superseded by not matching, and the next acknowledgement overwrites it.
--
-- Forward-only and additive, applied idempotently on every boot (ADR-0017).

-- One row per store, keyed as every other per-store row in this schema is.
--
-- **There is no `acknowledged_by` column, and that is deliberate rather than an omission.** The
-- neighbouring `store_lease.retired_at`/`retired_by` pair does carry its deciding admin, and the
-- difference is what the two rows are: `store_lease` is a store's standing operational record, while
-- this row is overwritten by the next acknowledgement. A "who" stored here would be destroyed by the
-- next person to answer a warning, and destroyed silently — whereas the audit action
-- `store.edge_placement.acknowledge` records every one of them, with the acting admin's id and email,
-- and cannot be overwritten. Storing it twice would create the copy that goes missing, so the trail
-- answers "who decided" and this row answers "what is currently answered".
--
-- `reason` is free text somebody typed and is stored verbatim, never parsed, never logged and never
-- put in an event payload — the same handling as `store_lease.region_label`, for the same reason: it
-- is the one field on this row where a person's name could land. The 280-character ceiling is
-- enforced at the write in Unicode scalar values, which a `varchar(280)` could not express, and the
-- refusal has to name the field and say what it accepts, which a truncation cannot.
--
-- `acknowledged_at` is milliseconds since the epoch as `bigint`, matching `store_lease.issued_at`
-- and `store_lease.retired_at` rather than introducing a second time representation into a schema
-- that has one.
CREATE TABLE IF NOT EXISTS store_region_acknowledgement (
    tenant_id       text   NOT NULL,
    store_id        text   NOT NULL,
    profile_country text   NOT NULL,
    region_country  text   NOT NULL,
    reason          text   NOT NULL,
    acknowledged_at bigint NOT NULL,
    PRIMARY KEY (tenant_id, store_id)
);

-- Tenant isolation on the shape `config_trees` uses, because this row is a tenant's record of a
-- decision about its own store and no other tenant has any business reading it.
ALTER TABLE store_region_acknowledgement ENABLE ROW LEVEL SECURITY;
DROP POLICY IF EXISTS store_region_acknowledgement_tenant_isolation ON store_region_acknowledgement;
CREATE POLICY store_region_acknowledgement_tenant_isolation ON store_region_acknowledgement
    USING (tenant_id = current_setting('app.tenant_id', true));
GRANT SELECT, INSERT, UPDATE ON store_region_acknowledgement TO app_tenant;
