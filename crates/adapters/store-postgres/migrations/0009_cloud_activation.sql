-- Copyright (c) 2026 Pizza 4P's. All rights reserved.
-- Proprietary and confidential. Internal use only. See LICENSE.

-- activation_codes + device_credentials: the once-only device activation exchange (P9, ADR-0050). A
-- machine is activated once — an operator types a short code, the cloud exchanges it for the
-- machine's long-lived credential, and the code is spent. Forward-only and additive, applied
-- idempotently on every boot (ADR-0017).
--
-- Neither table gets row-level security. Like api_keys (0002), a code is looked up by its hash and a
-- credential by its id — a global key known before any tenant is proven — so isolation rests on
-- binding the resulting action to the row's own tenant_id, not on an RLS predicate keyed off a
-- tenant the caller has not yet established.
--
--  * activation_codes.code_hash is SHA-256 of the canonical code; the code itself is never stored, so
--    a database dump leaks no live code. `status` is 'issued' at mint and moves once to 'redeemed'
--    (the exchange — single-use) or 'revoked' (an operator cancels a leaked setup sheet).
--    tenant_id/store_id/device_id are the slot the code — and the credential it mints — belong to.
--  * device_credentials.id is a ULID, the public half of the posdev_<id>_<secret> token; only
--    secret_hash (SHA-256 of the secret) is stored, and the secret is shown once at the exchange.
CREATE TABLE IF NOT EXISTS activation_codes (
    code_hash   bytea       PRIMARY KEY,
    tenant_id   text        NOT NULL,
    store_id    text        NOT NULL,
    device_id   text        NOT NULL,
    status      text        NOT NULL DEFAULT 'issued',
    created_at  timestamptz NOT NULL DEFAULT now(),
    redeemed_at timestamptz
);
-- Cancelling a device's pending activation (revoke) filters live codes by their slot; a partial
-- index answers it without scanning spent rows.
CREATE INDEX IF NOT EXISTS activation_codes_issued_slot
    ON activation_codes (tenant_id, store_id, device_id) WHERE status = 'issued';

CREATE TABLE IF NOT EXISTS device_credentials (
    id          text        PRIMARY KEY,
    tenant_id   text        NOT NULL,
    store_id    text        NOT NULL,
    device_id   text        NOT NULL,
    secret_hash bytea       NOT NULL,
    created_at  timestamptz NOT NULL DEFAULT now()
);
-- A device slot's credentials, for an operator view and for revocation on a machine swap.
CREATE INDEX IF NOT EXISTS device_credentials_slot
    ON device_credentials (tenant_id, store_id, device_id);
