-- Copyright (c) 2026 Pizza 4P's. All rights reserved.
-- Proprietary and confidential. Internal use only. See LICENSE.

-- The super-admin credential and its server-side session table (P7, ADR-0034). Neither is tenant
-- data: there is exactly one super-admin for the whole cloud, and its sessions authorise the
-- privileged admin surface, not a tenant's `/v1` data. So — unlike the event log and the rollups —
-- these tables carry no tenant_id, no RLS, and no grant to the per-tenant `app_tenant` role: only the
-- trusted connection (the pool owner) ever touches them. Forward-only and additive, applied
-- idempotently on every boot (ADR-0017).

-- The single super-admin credential. `id boolean PRIMARY KEY DEFAULT true` with a CHECK pins the
-- table to exactly one row: a second insert collides on the primary key, so there can never be two
-- super-admins. `password_phc` is the Argon2id PHC string (the hash, never the password);
-- `totp_secret` is the raw RFC 6238 shared secret (bytea); `last_used_totp_step` is the newest TOTP
-- step already spent — NULL until the first login, and only ever advanced forward, which is what
-- makes a code single-use across restarts. Provisioning the row (enrolment / QR) is a later slice
-- (ADR-0034); this migration ships the shape it lives in.
CREATE TABLE IF NOT EXISTS super_admin (
    id                  boolean     PRIMARY KEY DEFAULT true,
    password_phc        text        NOT NULL,
    totp_secret         bytea       NOT NULL,
    last_used_totp_step bigint,
    updated_at          timestamptz NOT NULL DEFAULT now(),
    CONSTRAINT super_admin_single_row CHECK (id)
);

-- The server-side session table. The `__Host-` session cookie carries a 256-bit random token; this
-- stores only `SHA-256(token)` (bytea, 32 bytes) as the primary key, so a read of this table yields
-- no usable session — the same posture as the API-key secret hash (ADR-0037). `expires_at` is
-- milliseconds since the Unix epoch, matching the domain `Timestamp` exactly so no timezone rounding
-- can move an expiry boundary; a session is live only while `expires_at > now`. Logout deletes the
-- row; expired rows are swept by a later maintenance slice (until then the `expires_at` check already
-- makes them unusable).
CREATE TABLE IF NOT EXISTS admin_sessions (
    token_hash bytea       PRIMARY KEY,
    expires_at bigint      NOT NULL,
    created_at timestamptz NOT NULL DEFAULT now()
);
-- Answers the sweep of expired sessions (a later slice) without a full scan.
CREATE INDEX IF NOT EXISTS admin_sessions_expiry ON admin_sessions (expires_at);
