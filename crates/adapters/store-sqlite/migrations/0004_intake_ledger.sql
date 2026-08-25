-- Copyright (c) 2026 Pizza 4P's. All rights reserved.
-- Proprietary and confidential. Internal use only. See LICENSE.
--
-- 0004 — the inbound-order idempotency ledger (ADR-0064, the edge `OrderIn` authority).
--
-- An inbound order (a marketplace order, the public API, a QR guest) carries the CALLER's own
-- reference — a marketplace order id, an idempotency key — which is not in the event log. This is
-- the side record that maps that reference to what the order became, so a retry or the relay's
-- at-least-once redelivery returns the same acceptance instead of opening a second order.
--
-- The row is written IN THE SAME TRANSACTION as sales.order.opened (ADR-0064), so either the order
-- and its ledger row both land or neither does — the guarantee that makes a crash between the two
-- impossible. The PRIMARY KEY is what enforces it: a second order racing in on the same key hits a
-- constraint violation on commit and rolls back (the one writer thread serialises the two), rather
-- than duplicating. `record` is the JSON `IntakeRecord` the caller rebuilds the acceptance from.
--
-- Additive-only (ADR-0017): immutable once merged. A change is a new numbered file.

CREATE TABLE intake_ledger (
    store_id           TEXT NOT NULL,
    sales_channel      TEXT NOT NULL,
    external_reference TEXT NOT NULL,
    record             TEXT NOT NULL,
    PRIMARY KEY (store_id, sales_channel, external_reference)
) WITHOUT ROWID;
