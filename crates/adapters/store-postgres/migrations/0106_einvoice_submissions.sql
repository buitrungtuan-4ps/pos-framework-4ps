-- Copyright (c) 2026 Pizza 4P's. All rights reserved.
-- Proprietary and confidential. Internal use only. See LICENSE.
--
-- 0106 — the cloud's e-invoice submissions and the feed that fills them
-- ([ADR-0241](../../../../docs/adr/0241-the-cloud-submits-each-issued-invoice-from-the-log-and-keeps-its-state.md)).
--
-- `einvoice_submissions`: one row per invoice a store issued (`fiscal.invoice.issued`) or the cloud
-- recorded as a correction, keyed by the invoice's id.
--
--   * `invoice_number`      — the symbol and sequence, `1C26MAA/00000042`.
--   * `kind`                — `ORIGINAL`, `ADJUSTMENT` or `REPLACEMENT`.
--   * `issued_on`           — the invoice's calendar date, `YYYY-MM-DD`.
--   * `total_minor`, `currency` — the total, in the currency's minor unit.
--   * `buyer_subject_id`    — the buyer's subject id, never the buyer's details.
--   * `state`               — `PENDING`, `RETRYING`, `SUBMITTED` or `REJECTED`.
--   * `next_attempt_ms`, `failed_attempts`, `authority_reference`, `refusal` — the submission.
--   * `corrects_invoice_number` — for a correction, the number of the invoice it corrects.
--
-- `einvoice_feed_cursors`: the last event of each store's log the feed has read.
--
-- RETENTION: kept with the invoice it records, whose retention legal must state (ADR-0225 decision
-- 4). No personal data: the buyer is named by subject id only.
--
-- Tenant-scoped under RLS like the rest. Forward-only and additive, applied idempotently on every
-- boot (ADR-0017).

CREATE TABLE IF NOT EXISTS einvoice_submissions (
    tenant_id           text   NOT NULL,
    invoice_id          text   NOT NULL,
    store_id            text   NOT NULL,
    bill_id             text   NOT NULL,
    invoice_number      text   NOT NULL,
    kind                text   NOT NULL,
    issued_on           text   NOT NULL,
    total_minor         bigint NOT NULL,
    currency            text   NOT NULL,
    buyer_subject_id    text,
    connection_id       text,
    corrects_invoice_number text,
    state               text   NOT NULL,
    failed_attempts     integer NOT NULL DEFAULT 0,
    next_attempt_ms     bigint NOT NULL,
    authority_reference text,
    refusal             text,
    recorded_at_ms      bigint NOT NULL,
    updated_at_ms       bigint NOT NULL,
    PRIMARY KEY (tenant_id, invoice_id)
);
CREATE INDEX IF NOT EXISTS einvoice_submissions_due
    ON einvoice_submissions (next_attempt_ms) WHERE state IN ('PENDING', 'RETRYING');
CREATE INDEX IF NOT EXISTS einvoice_submissions_by_store
    ON einvoice_submissions (tenant_id, store_id, recorded_at_ms);

CREATE TABLE IF NOT EXISTS einvoice_feed_cursors (
    tenant_id     text NOT NULL,
    store_id      text NOT NULL,
    last_event_id text NOT NULL,
    PRIMARY KEY (tenant_id, store_id)
);

ALTER TABLE einvoice_submissions ENABLE ROW LEVEL SECURITY;
DROP POLICY IF EXISTS einvoice_submissions_tenant_isolation ON einvoice_submissions;
CREATE POLICY einvoice_submissions_tenant_isolation ON einvoice_submissions
    USING (tenant_id = current_setting('app.tenant_id', true));
GRANT SELECT, INSERT, UPDATE ON einvoice_submissions TO app_tenant;

ALTER TABLE einvoice_feed_cursors ENABLE ROW LEVEL SECURITY;
DROP POLICY IF EXISTS einvoice_feed_cursors_tenant_isolation ON einvoice_feed_cursors;
CREATE POLICY einvoice_feed_cursors_tenant_isolation ON einvoice_feed_cursors
    USING (tenant_id = current_setting('app.tenant_id', true));
GRANT SELECT, INSERT, UPDATE ON einvoice_feed_cursors TO app_tenant;
