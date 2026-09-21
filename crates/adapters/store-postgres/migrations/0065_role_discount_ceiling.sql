-- Copyright (c) 2026 Pizza 4P's. All rights reserved.
-- Proprietary and confidential. Internal use only. See LICENSE.
--
-- 0065 — the discount ceiling a role carries.
--
-- `billing.discount.apply` has been published since the permission catalogue was written, granted to
-- a **server**, not PIN-flagged, and described as *"apply a discount up to the role's configured
-- ceiling"*. There was nowhere to configure one. Nothing in the console authored it, no column held
-- it and no published node carried it, so the only thing bounding a permission a server holds
-- without a PIN did not exist.
--
-- The edge reads an absent ceiling as **zero** rather than as "no limit" — the safe direction, and
-- the one already shipped and tested — so today every discount needs
-- `billing.discount.override_ceiling` and a manager's PIN. This column is what lets a tenant say
-- otherwise, and the day it is set a server's small discount goes through with no code change.
--
-- **On the role, not on the person.** The permission's own description says *the role's* ceiling,
-- and a tenant's *Cashier* is one policy rather than forty. A person who needs a different bound
-- needs a different role, which is also how the audit trail reads afterwards.
--
-- **Minor units, as `bigint`.** Money is an integer in the currency's minor unit everywhere in this
-- system and a ceiling is no exception; there is no currency column beside it because a store's
-- currency comes from its `locale` node and a role does not have one of its own.
--
-- **NULL is not zero here.** NULL means "this role has no configured ceiling", which the edge reads
-- as zero-and-needs-a-manager; a stored `0` means somebody deliberately said this role discounts
-- nothing. The two land in the same place today, and they are different statements — the first is
-- an absence, the second is a policy. Keeping them apart costs a nullable column and means a console
-- can show "not set" rather than inventing a number the operator never typed.
--
-- Forward-only and additive, applied idempotently on every boot (ADR-0017). A role that predates it
-- reads NULL, which is exactly the behaviour every store has today.

ALTER TABLE role_templates
    ADD COLUMN IF NOT EXISTS discount_ceiling_minor bigint;

-- A ceiling below zero is not a smaller allowance, it is a nonsense: the domain compares
-- `ceiling - amount` and a negative ceiling would make every discount an override, including one of
-- nothing. Refused at the column so no write path can introduce it, whatever the route forgets.
ALTER TABLE role_templates
    DROP CONSTRAINT IF EXISTS role_templates_discount_ceiling_not_negative;
ALTER TABLE role_templates
    ADD CONSTRAINT role_templates_discount_ceiling_not_negative
    CHECK (discount_ceiling_minor IS NULL OR discount_ceiling_minor >= 0);
