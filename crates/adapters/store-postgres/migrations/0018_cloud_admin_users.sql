-- Copyright (c) 2026 Pizza 4P's. All rights reserved.
-- Proprietary and confidential. Internal use only. See LICENSE.

-- Multi-admin console identities with roles ([ADR-0067], superseding the single-row super_admin of
-- ADR-0034). Like `super_admin` and `admin_sessions` (migration 0003), none of this is tenant data:
-- these identities authorise the privileged console, not a tenant's `/v1` data, so — unlike the event
-- log and rollups — the tables carry no tenant_id, no RLS, and no grant to the per-tenant
-- `app_tenant` role; only the trusted pool owner touches them. Forward-only and additive, applied
-- idempotently on every boot (ADR-0017). This migration ships the shape multi-admin lives in and
-- seeds the existing super_admin as the first `owner`; the login flow and role-aware guard migrate to
-- these tables in later G1 slices, so nothing here changes runtime behaviour on its own.

-- One row per named console admin. `id` is a ULID string minted at the HTTP edge (the same posture as
-- every other cloud id). `email` is the login identity — a case-insensitive unique index enforces
-- one account per address regardless of case. `password_phc` (Argon2id) and `totp_secret` (raw RFC
-- 6238 secret) are per-user: no admin ever learns another's, exactly as ADR-0067 requires; only their
-- hash/secret is stored, never a plaintext password. `last_used_totp_step` is the newest TOTP step
-- spent, advanced only forward so a code is single-use across restarts (unchanged from ADR-0034,
-- now per admin). `role` and `status` are constrained to the fixed vocabularies; the role→permission
-- templates live in `pos-core`-style code (a later slice), the CHECK here only guards the storage.
-- The "there is always at least one active owner" invariant is enforced in application code
-- (count_active_owners), not a table constraint, because it spans rows.
CREATE TABLE IF NOT EXISTS admin_users (
    id                  text        PRIMARY KEY,
    email               text        NOT NULL,
    name                text        NOT NULL,
    role                text        NOT NULL,
    status              text        NOT NULL DEFAULT 'active',
    password_phc        text        NOT NULL,
    totp_secret         bytea       NOT NULL,
    last_used_totp_step bigint,
    created_at          timestamptz NOT NULL DEFAULT now(),
    updated_at          timestamptz NOT NULL DEFAULT now(),
    CONSTRAINT admin_users_role_vocab   CHECK (role IN ('owner', 'admin', 'ops', 'viewer')),
    CONSTRAINT admin_users_status_vocab CHECK (status IN ('active', 'suspended'))
);
-- One account per email, case-insensitively: `Owner@x` and `owner@x` are the same identity.
CREATE UNIQUE INDEX IF NOT EXISTS admin_users_email_key ON admin_users (lower(email));

-- Pending invitations. An owner/admin invites by email; the server mints a single-use, TTL-bounded
-- token and stores only its `SHA-256` (bytea, 32 bytes) — the raw token reaches the invitee once and
-- is never persisted, the same posture as the session token and the API-key secret. The invitee
-- exchanges it to set their own password and enrol TOTP, then a row here is marked `accepted_at`.
-- `invited_by` references the inviting admin. `expires_at`/`accepted_at` are Unix milliseconds,
-- matching the domain `Timestamp` exactly (no timezone rounding can move a boundary).
CREATE TABLE IF NOT EXISTS admin_invites (
    id          text        PRIMARY KEY,
    email       text        NOT NULL,
    name        text        NOT NULL,
    role        text        NOT NULL,
    token_hash  bytea       NOT NULL UNIQUE,
    invited_by  text        NOT NULL REFERENCES admin_users (id),
    expires_at  bigint      NOT NULL,
    accepted_at bigint,
    created_at  timestamptz NOT NULL DEFAULT now(),
    CONSTRAINT admin_invites_role_vocab CHECK (role IN ('owner', 'admin', 'ops', 'viewer'))
);
-- Resolve a still-pending invitation for an email without a full scan.
CREATE INDEX IF NOT EXISTS admin_invites_email ON admin_invites (lower(email));

-- One-time recovery codes for a lost authenticator, an alternative to the ADR-0045 break-glass. Each
-- code is stored only as its `SHA-256` (bytea, 32 bytes); `used_at` (Unix ms) burns it single-use.
-- Deleting an admin cascades their codes.
CREATE TABLE IF NOT EXISTS admin_recovery_codes (
    id         text        PRIMARY KEY,
    admin_id   text        NOT NULL REFERENCES admin_users (id) ON DELETE CASCADE,
    code_hash  bytea       NOT NULL,
    used_at    bigint,
    created_at timestamptz NOT NULL DEFAULT now(),
    CONSTRAINT admin_recovery_codes_unique UNIQUE (admin_id, code_hash)
);
CREATE INDEX IF NOT EXISTS admin_recovery_codes_admin ON admin_recovery_codes (admin_id);

-- `admin_sessions` gains per-admin accountability: which admin the session belongs to, and the client
-- IP and user-agent it was minted for, so an admin can list and revoke their live sessions. Additive
-- and nullable so the existing single-admin sessions (minted before this migration) stay valid; the
-- session guard keeps working through the transition. `ON DELETE CASCADE` drops an admin's sessions
-- with the admin. `ADD COLUMN IF NOT EXISTS` keeps the migration idempotent.
ALTER TABLE admin_sessions ADD COLUMN IF NOT EXISTS admin_id   text REFERENCES admin_users (id) ON DELETE CASCADE;
ALTER TABLE admin_sessions ADD COLUMN IF NOT EXISTS ip         text;
ALTER TABLE admin_sessions ADD COLUMN IF NOT EXISTS user_agent text;
-- List an admin's live sessions without a full scan.
CREATE INDEX IF NOT EXISTS admin_sessions_admin ON admin_sessions (admin_id);

-- Migrate the existing single super_admin into the first `owner` admin_user, in place, so an
-- installation upgrades without losing its credential and there is always at least one owner. The
-- super_admin row carries no email or name (its account label was literally "super-admin"), so the
-- seed uses a synthetic, non-routable placeholder address the owner replaces from the console; the
-- password hash and TOTP secret carry over verbatim, so the owner signs in with exactly the
-- credential they already have. The all-zeros ULID is a stable sentinel id for the migrated owner.
-- `super_admin` is deliberately left in place (not dropped) — this is additive and the login flow
-- still reads it until a later slice switches over; ADR-0045 `reset_admin` therefore still works. The
-- guarded INSERT/`ON CONFLICT DO NOTHING` makes the backfill run at most once and idempotent on
-- re-boot.
INSERT INTO admin_users (id, email, name, role, status, password_phc, totp_secret, last_used_totp_step)
SELECT
    '00000000000000000000000000',
    'owner@super-admin.invalid',
    'Owner',
    'owner',
    'active',
    password_phc,
    totp_secret,
    last_used_totp_step
FROM super_admin
WHERE id
-- Bare `ON CONFLICT DO NOTHING` so the backfill is a no-op on re-boot whether the sentinel id or the
-- placeholder email is what already exists — it never raises, and never duplicates the owner.
ON CONFLICT DO NOTHING;
