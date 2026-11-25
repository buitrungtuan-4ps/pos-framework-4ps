-- Copyright (c) 2026 Pizza 4P's. All rights reserved.
-- Proprietary and confidential. Internal use only. See LICENSE.
--
-- 0107 — an e-invoice number is recorded once per tenant
-- ([ADR-0241](../../../../docs/adr/0241-the-cloud-submits-each-issued-invoice-from-the-log-and-keeps-its-state.md)
-- decision 5): a correction the cloud numbers under the tenant's correction symbol can never take a
-- number twice, even from two consoles at once. Forward-only and additive (ADR-0017).

CREATE UNIQUE INDEX IF NOT EXISTS einvoice_submissions_number
    ON einvoice_submissions (tenant_id, invoice_number);
