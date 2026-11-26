-- Copyright (c) 2026 Pizza 4P's. All rights reserved.
-- Proprietary and confidential. Internal use only. See LICENSE.
--
-- 0109 — the queue of correction requests a store manager sent from the till
-- ([ADR-0250](../../../../docs/adr/0250-a-store-learns-a-correction-decision-on-its-einvoice-node.md)
-- decision 3): the cloud's e-invoice feed records each `fiscal.correction.requested` here, and HQ or
-- the accountant approves or declines it in the console.
--
--   * `request_id`         — the `fiscal.correction.requested` event's id, so a request read twice
--                            is recorded once.
--   * `store_id`, `bill_id` — where, and which bill.
--   * `amount_minor`, `currency_code` — the amount to take back, in the currency's minor unit.
--   * `kind`               — `ADJUSTMENT` or `REPLACEMENT`, as the store asked.
--   * `reason_code_id`     — the `VOID_BILL` reason code, where one was given.
--   * `requested_by`       — the requesting employee's id (no name).
--   * `requested_time_ms`  — when the till recorded it, in Unix milliseconds.
--   * `status`             — `PENDING`, `APPROVED` or `DECLINED`.
--   * `decided_by_user_id`, `decided_time_ms` — the console user (by id) and when, once decided.
--
-- RETENTION: kept with the tenant's e-invoices, since it is part of an invoice's audit trail. No
-- personal data: people are named by id only.
--
-- Tenant-scoped under RLS like the rest. Forward-only and additive, applied idempotently on every
-- boot (ADR-0017).

CREATE TABLE IF NOT EXISTS einvoice_correction_requests (
    tenant_id          text   NOT NULL,
    request_id         text   NOT NULL,
    store_id           text   NOT NULL,
    bill_id            text   NOT NULL,
    amount_minor       bigint NOT NULL,
    currency_code      text   NOT NULL,
    kind               text   NOT NULL,
    reason_code_id     text,
    requested_by       text,
    requested_time_ms  bigint NOT NULL,
    status             text   NOT NULL,
    decided_by_user_id text,
    decided_time_ms    bigint,
    PRIMARY KEY (tenant_id, request_id)
);
CREATE INDEX IF NOT EXISTS einvoice_correction_requests_by_status
    ON einvoice_correction_requests (tenant_id, status, requested_time_ms);

ALTER TABLE einvoice_correction_requests ENABLE ROW LEVEL SECURITY;
DROP POLICY IF EXISTS einvoice_correction_requests_tenant_isolation ON einvoice_correction_requests;
CREATE POLICY einvoice_correction_requests_tenant_isolation ON einvoice_correction_requests
    USING (tenant_id = current_setting('app.tenant_id', true));
GRANT SELECT, INSERT, UPDATE ON einvoice_correction_requests TO app_tenant;
