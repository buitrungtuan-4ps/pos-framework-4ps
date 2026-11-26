-- Copyright (c) 2026 Pizza 4P's. All rights reserved.
-- Proprietary and confidential. Internal use only. See LICENSE.
--
-- 0108 — a bill's invoice is found by its bill
-- ([ADR-0245](../../../../docs/adr/0245-an-invoiced-bill-is-corrected-by-the-route-the-store-chooses.md)):
-- when a store voids a bill whose invoice was issued, the cloud's feed reads the bill's original
-- invoice to correct it. Forward-only and additive (ADR-0017).

CREATE INDEX IF NOT EXISTS einvoice_submissions_by_bill
    ON einvoice_submissions (tenant_id, store_id, bill_id);
