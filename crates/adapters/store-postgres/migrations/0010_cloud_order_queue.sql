-- Copyright (c) 2026 Pizza 4P's. All rights reserved.
-- Proprietary and confidential. Internal use only. See LICENSE.

-- Order queue: the cloud's durable inbox of orders bound for a store's POS (P7). Unlike a webhook
-- (0006), which is a cursor over the event log and holds no backlog, a row here IS a backlog item: a
-- sales channel offers an order, it is queued here idempotently, the store's device pulls the pending
-- orders, and the device reports each one's outcome back. Forward-only and additive, applied
-- idempotently on every boot (ADR-0017).
--
--  * `(tenant_id, store_id, sales_channel, external_reference)` is the idempotency key and the primary
--    key: the same order re-offered by a channel lands on the row already there, never a duplicate.
--  * `queued_id` is a ULID, the cloud's own handle for the queued order, unique within a tenant; the
--    device names it when reporting, so the outcome report addresses exactly one row.
--  * `payload` is the order as the channel offered it. Derived and mutable, so `jsonb` (unlike the
--    event log's byte-preserving `json`), the same choice the config tree and rollups make.
--  * `status` is 'pending' at enqueue and moves once to 'reported' when the device records an outcome.
--  * `outcome` is NULL until reported, then the device's result for the order (`jsonb`).
--  * `tenant_id` scopes the queue to its tenant (RLS + the explicit tenant filter every query carries),
--    the same posture as the config tree and the webhook admin CRUD.
CREATE TABLE IF NOT EXISTS order_queue (
    tenant_id          text        NOT NULL,
    store_id           text        NOT NULL,
    sales_channel      text        NOT NULL,
    external_reference text        NOT NULL,
    queued_id          text        NOT NULL,
    payload            jsonb       NOT NULL,
    status             text        NOT NULL DEFAULT 'pending',
    outcome            jsonb,
    created_at         timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (tenant_id, store_id, sales_channel, external_reference)
);
-- The outcome report addresses a row by its queued_id; a unique index per tenant enforces that a
-- queued_id names exactly one order and answers the report without a scan.
CREATE UNIQUE INDEX IF NOT EXISTS order_queue_tenant_queued ON order_queue (tenant_id, queued_id);
-- The device pulls its store's pending orders oldest-first; this index answers the pull without a scan.
CREATE INDEX IF NOT EXISTS order_queue_pending ON order_queue (tenant_id, store_id, status, created_at);

ALTER TABLE order_queue ENABLE ROW LEVEL SECURITY;
DROP POLICY IF EXISTS order_queue_tenant_isolation ON order_queue;
CREATE POLICY order_queue_tenant_isolation ON order_queue
    USING (tenant_id = current_setting('app.tenant_id', true));
GRANT SELECT, INSERT, UPDATE ON order_queue TO app_tenant;
