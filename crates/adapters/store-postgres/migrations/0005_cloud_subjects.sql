-- Copyright (c) 2026 Pizza 4P's. All rights reserved.
-- Proprietary and confidential. Internal use only. See LICENSE.

-- The subject store: the one place personal data lives (P7 / Track A6, ADR-0035). Events never carry
-- PII (ADR — pos_proto::pii); a marketplace order's name/phone/address and a corporate invoice's
-- buyer fields live here, keyed by a globally-unique subject_id (a ULID), and events reference only
-- that id. So "anonymise a person" is "mask one row here", and because every money figure is in the
-- events, the books still reconcile after masking. Forward-only and additive, applied idempotently on
-- every boot (ADR-0017).
--
--  * `subject_id` is the primary key — a ULID, unique across the fleet, so it needs no partitioning
--    and a mask/lookup is a single-row touch.
--  * `collected_at` and `masked_at` are milliseconds since the Unix epoch, matching the domain
--    `Timestamp` exactly; the retention clock runs from `collected_at`, and `masked_at` is NULL while
--    the row still holds personal data (the retention sweep reads only NULL-`masked_at` rows, which is
--    what makes it idempotent — a masked row is never handed back).
--  * `fields` is the personal data proper (jsonb: {name, phone, address, email, tax_code, …}); masking
--    overwrites every value with the redaction sentinel in place, so the PII is gone from the row, not
--    merely flagged.
--  * `tenant_id` scopes the PII to its tenant. The retention cron sweeps the whole fleet as the
--    trusted (owner) role, so it bypasses RLS; a future tenant-scoped read path assuming app_tenant is
--    filtered to its own — the same posture as the rollups and config-tree tables, and the right
--    default for a table of personal data.
CREATE TABLE IF NOT EXISTS subjects (
    subject_id   text        PRIMARY KEY,
    tenant_id    text        NOT NULL,
    collected_at bigint      NOT NULL,
    fields       jsonb       NOT NULL,
    masked_at    bigint,
    created_at   timestamptz NOT NULL DEFAULT now()
);
-- Answers the retention sweep's "unmasked and collected at or before the cutoff" query without a scan.
CREATE INDEX IF NOT EXISTS subjects_unmasked_by_collected
    ON subjects (collected_at) WHERE masked_at IS NULL;

ALTER TABLE subjects ENABLE ROW LEVEL SECURITY;
DROP POLICY IF EXISTS subjects_tenant_isolation ON subjects;
CREATE POLICY subjects_tenant_isolation ON subjects
    USING (tenant_id = current_setting('app.tenant_id', true));
GRANT SELECT, INSERT, UPDATE ON subjects TO app_tenant;
