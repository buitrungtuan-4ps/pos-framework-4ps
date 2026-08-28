-- Copyright (c) 2026 Pizza 4P's. All rights reserved.
-- Proprietary and confidential. Internal use only. See LICENSE.

-- Voucher instances minted for a voucher-kind campaign (Track M3, ADR-0077). One row per code an
-- operator generated and hands out; redemption (the engine's online check-and-mark, the
-- PromotionVoucher* events) is a runtime concern that later flips `status`. Minting and listing the
-- codes is what M3 adds.
--
-- Unlike an API key (0009/ADR-0037), the `code` is stored in clear text: a voucher code is meant to
-- be distributed — printed on a flyer, e-mailed to a guest — so the operator must read it back to hand
-- it out. It is still sensitive (redeemable value), so the console gates listing behind the same
-- manage permission that mints it, never plain read. `(tenant_id, code)` is unique so a code resolves
-- to one voucher within a tenant; codes are not globally unique across tenants (redemption is within a
-- store). `campaign_id` references a `campaigns` row (application-enforced, the no-FK posture every
-- cloud table keeps).
--
-- Tenant-scoped exactly like the rest of the config data (0028/0032): RLS on `app.tenant_id`, a grant
-- to `app_tenant`, the trusted pool owner bypassing RLS. UPDATE is granted for the runtime redemption
-- path that later flips `status`. Forward-only and additive, applied idempotently on every boot
-- (ADR-0017). Greenfield — no backfill.

CREATE TABLE IF NOT EXISTS vouchers (
    tenant_id   text        NOT NULL,
    voucher_id  text        NOT NULL,
    campaign_id text        NOT NULL,
    code        text        NOT NULL,
    status      text        NOT NULL DEFAULT 'ACTIVE',
    created_at  timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (tenant_id, voucher_id),
    CONSTRAINT vouchers_code_unique UNIQUE (tenant_id, code)
);
CREATE INDEX IF NOT EXISTS vouchers_by_campaign ON vouchers (tenant_id, campaign_id);

ALTER TABLE vouchers ENABLE ROW LEVEL SECURITY;
DROP POLICY IF EXISTS vouchers_tenant_isolation ON vouchers;
CREATE POLICY vouchers_tenant_isolation ON vouchers
    USING (tenant_id = current_setting('app.tenant_id', true));
GRANT SELECT, INSERT, UPDATE, DELETE ON vouchers TO app_tenant;
