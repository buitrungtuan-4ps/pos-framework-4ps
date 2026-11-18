-- Copyright (c) 2026 Pizza 4P's. All rights reserved.
-- Proprietary and confidential. Internal use only. See LICENSE.
--
-- 0103 — an item's barcodes, PLU and open price
-- ([ADR-0222](../../../../docs/adr/0222-items-carry-barcodes-and-plus-and-a-counter-can-pay-first.md)
-- decisions 1 and 4).
--
-- One jsonb object, `{"barcodes": [...], "plu": "...", "open_price": {"minimum": ..., "maximum": ...}}`,
-- each key absent when the item has none, as the item's names are kept. The check digits, the
-- lengths and a code's uniqueness within a tenant's menu are checked at the write by the catalogue
-- routes, which can name both items a duplicate falls on, where a constraint could only say one
-- was violated. `{}` is every item today and stays correct.
--
-- RETENTION: unchanged; catalogue data, no personal data. Forward-only and additive, applied
-- idempotently on every boot (ADR-0017).

ALTER TABLE catalog_items
    ADD COLUMN IF NOT EXISTS codes jsonb NOT NULL DEFAULT '{}'::jsonb;
