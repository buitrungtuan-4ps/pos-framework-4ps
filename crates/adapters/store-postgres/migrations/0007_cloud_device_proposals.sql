-- Copyright (c) 2026 Pizza 4P's. All rights reserved.
-- Proprietary and confidential. Internal use only. See LICENSE.

-- Device proposals: the discover→propose→admin-approves onboarding queue (P7, ADR-0041). A store
-- discovers a printer or KDS on its LAN and proposes it; a super-admin approves it before it is
-- usable — the human gate that keeps an unauthenticated port-9100 device off the fleet. Forward-only
-- and additive, applied idempotently on every boot (ADR-0017).
--
--  * `id` is a ULID, the primary key, used to address the proposal in the admin approve/reject routes.
--  * `kind` is 'printer' or 'kds'; `name` and `address` are the discovered facts the store submitted.
--  * `status` is 'pending' at proposal, and moves once to 'approved' or 'rejected' by an operator;
--    the store reads back only its 'approved' devices, so the edge never acts on a raw discovery.
--  * `tenant_id` scopes the proposal (RLS + the admin queue's and store listing's tenant filter);
--    `store_id` is the store that discovered it.
CREATE TABLE IF NOT EXISTS device_proposals (
    id          text        PRIMARY KEY,
    tenant_id   text        NOT NULL,
    store_id    text        NOT NULL,
    kind        text        NOT NULL,
    name        text        NOT NULL,
    address     text        NOT NULL,
    status      text        NOT NULL DEFAULT 'pending',
    created_at  timestamptz NOT NULL DEFAULT now(),
    resolved_at timestamptz
);
-- Listing a tenant's proposals (admin queue, store's approved list) filters by tenant.
CREATE INDEX IF NOT EXISTS device_proposals_tenant ON device_proposals (tenant_id);
-- The admin pending queue is the hot read; a partial index answers it without scanning resolved rows.
CREATE INDEX IF NOT EXISTS device_proposals_pending
    ON device_proposals (tenant_id, created_at) WHERE status = 'pending';

ALTER TABLE device_proposals ENABLE ROW LEVEL SECURITY;
DROP POLICY IF EXISTS device_proposals_tenant_isolation ON device_proposals;
CREATE POLICY device_proposals_tenant_isolation ON device_proposals
    USING (tenant_id = current_setting('app.tenant_id', true));
GRANT SELECT, INSERT, UPDATE, DELETE ON device_proposals TO app_tenant;
