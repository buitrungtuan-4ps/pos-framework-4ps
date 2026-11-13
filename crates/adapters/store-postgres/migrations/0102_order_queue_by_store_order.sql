-- Copyright (c) 2026 Pizza 4P's. All rights reserved.
-- Proprietary and confidential. Internal use only. See LICENSE.
--
-- 0102 — a relayed order found by the id its store gave it
-- ([ADR-0215](../../../../docs/adr/0215-a-delivery-order-names-its-vendor.md) decision 4).
--
-- A store answers a delivery vendor about an order it took by that order's own id: the vendor's
-- reference is the cloud's to look up, in the outcome the store reported. This index answers
-- "which reference did this store accept as this order" without a scan.
--
-- RETENTION: unchanged; an index of the queue's own rows. No personal data. Forward-only and
-- additive, applied idempotently on every boot (ADR-0017).

CREATE INDEX IF NOT EXISTS order_queue_by_store_order
    ON order_queue (tenant_id, store_id, (outcome ->> 'order_id'))
    WHERE status = 'reported';
