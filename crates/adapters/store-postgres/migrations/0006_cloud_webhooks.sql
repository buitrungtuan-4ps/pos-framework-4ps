-- Copyright (c) 2026 Pizza 4P's. All rights reserved.
-- Proprietary and confidential. Internal use only. See LICENSE.

-- Webhook endpoints: the durable facts of a subscription (P7, ADR-0032). A webhook is a cursor over
-- the event log, not a queue, so a row holds only where the endpoint points and how far it has got —
-- never a backlog. Forward-only and additive, applied idempotently on every boot (ADR-0017).
--
--  * `id` is a ULID, the primary key, used to address the endpoint in the admin routes.
--  * `secret` is the per-endpoint HMAC signing secret. Unlike an API-key secret (stored as a hash,
--    ADR-0037), this is kept in full: the cloud *signs* every delivery with it, so it must be
--    recoverable. It is shown to the tenant once at registration; the DB is its server-side home.
--  * `cursor` is the last delivered event_id (a ULID), NULL until the first delivery; it advances
--    only on a successful delivery, which is what bounds a dead endpoint's cost to nothing.
--  * `disabled` is set when the breaker auto-disables an endpoint after a day of continuous failure;
--    it clears only when an operator re-enables.
--  * `tenant_id` scopes the subscription to its tenant (RLS + the admin CRUD's tenant filter). The
--    delivery task loads enabled endpoints fleet-wide as the trusted role (RLS bypassed), the same
--    posture as the rollup projector and the retention sweep.
CREATE TABLE IF NOT EXISTS webhook_endpoints (
    id         text        PRIMARY KEY,
    tenant_id  text        NOT NULL,
    store_id   text        NOT NULL,
    url        text        NOT NULL,
    secret     text        NOT NULL,
    cursor     text,
    disabled   boolean     NOT NULL DEFAULT false,
    created_at timestamptz NOT NULL DEFAULT now()
);
-- Listing a tenant's endpoints (the admin CRUD) filters by tenant.
CREATE INDEX IF NOT EXISTS webhook_endpoints_tenant ON webhook_endpoints (tenant_id);
-- The delivery task loads the enabled endpoints; a partial index answers it without a scan.
CREATE INDEX IF NOT EXISTS webhook_endpoints_enabled ON webhook_endpoints (id) WHERE disabled = false;

ALTER TABLE webhook_endpoints ENABLE ROW LEVEL SECURITY;
DROP POLICY IF EXISTS webhook_endpoints_tenant_isolation ON webhook_endpoints;
CREATE POLICY webhook_endpoints_tenant_isolation ON webhook_endpoints
    USING (tenant_id = current_setting('app.tenant_id', true));
GRANT SELECT, INSERT, UPDATE, DELETE ON webhook_endpoints TO app_tenant;
