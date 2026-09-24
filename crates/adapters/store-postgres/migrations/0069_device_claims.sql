-- Copyright (c) 2026 Pizza 4P's. All rights reserved.
-- Proprietary and confidential. Internal use only. See LICENSE.

-- device_claims: a box installed with no store opens a claim and shows its code, a console user
-- binds the code to a device slot, and the box collects its device credential once (ADR-0148, the
-- device-authorisation pattern of RFC 8628). Forward-only and additive, applied idempotently on every
-- boot (ADR-0017).
--
-- No row-level security, like activation_codes and device_credentials (0009): a claim is found by
-- its id or by its code's hash, global keys known before any tenant is proven. The tenant is written
-- only when a console user with device rights binds it, and collection hands back exactly that slot.
--
--  * user_code_hash is SHA-256 of the canonical eight-character code; the code itself is never
--    stored. Unique, so one code names one claim; a collision fails the open and the box asks again.
--  * secret_hash is SHA-256 of the 256-bit secret only the box holds. Collection presents it.
--  * status moves once each way: 'pending' -> 'bound' (the console) -> 'collected' (the box).
--  * expires_at is Unix milliseconds, compared against the cloud's own clock, as sessions are
--    (0003). An expired claim is one whose hour has passed; nothing needs to sweep it for that.
--  * credential_id names the device_credentials row the collection minted.
CREATE TABLE IF NOT EXISTS device_claims (
    claim_id       text        PRIMARY KEY,
    user_code_hash bytea       NOT NULL UNIQUE,
    secret_hash    bytea       NOT NULL,
    status         text        NOT NULL DEFAULT 'pending',
    tenant_id      text,
    store_id       text,
    device_id      text,
    credential_id  text,
    expires_at     bigint      NOT NULL,
    created_at     timestamptz NOT NULL DEFAULT now(),
    bound_at       timestamptz,
    collected_at   timestamptz,
    CONSTRAINT device_claims_status CHECK (status IN ('pending', 'bound', 'collected'))
);
