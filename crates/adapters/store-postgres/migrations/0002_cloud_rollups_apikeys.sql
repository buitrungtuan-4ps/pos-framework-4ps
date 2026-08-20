-- Copyright (c) 2026 Pizza 4P's. All rights reserved.
-- Proprietary and confidential. Internal use only. See LICENSE.

-- Two tables the cloud maintains beside the event log (P7). Neither is the log — that is 0001's
-- append-only, byte-preserving, RLS-isolated source of truth — so both are mutable and use `jsonb`
-- where 0001 deliberately uses `json`. Forward-only and additive, applied idempotently on every boot
-- (ADR-0017), so every statement is `IF NOT EXISTS` / `OR REPLACE` / drop-then-create.

-- The materialised rollup read model (ADR-0036). One row per (tenant, store); `state` is the whole
-- StoredRollups — the projector's cursor plus the per-trading-day counts — as jsonb, so a dashboard
-- read is a single primary-key lookup, not a scan of the log, which is what makes the P7 <10 ms
-- answer real. Keyed by (tenant_id, store_id): a dashboard's tenant is its caller's authenticated
-- tenant, never a request parameter, so the key confines every read to the caller's own tenant.
CREATE TABLE IF NOT EXISTS rollups (
    tenant_id  text        NOT NULL,
    store_id   text        NOT NULL,
    state      jsonb       NOT NULL,
    updated_at timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (tenant_id, store_id)
);

-- The same tenant-isolation posture as the event log (ADR-0008/0016): a policy on tenant_id,
-- default-deny when the session sets no tenant. The trusted projector connects as the owner (RLS
-- bypassed) to maintain every tenant's rollup; a query role assuming app_tenant is filtered to its
-- own — belt-and-suspenders behind the (tenant_id, store_id) key the read already scopes by.
ALTER TABLE rollups ENABLE ROW LEVEL SECURITY;
DROP POLICY IF EXISTS rollups_tenant_isolation ON rollups;
CREATE POLICY rollups_tenant_isolation ON rollups
    USING (tenant_id = current_setting('app.tenant_id', true));
GRANT SELECT, INSERT, UPDATE ON rollups TO app_tenant;

-- Scoped per-tenant API keys (ADR-0037). Looked up by `id` — the public half of the
-- `pos_<id>_<secret>` token, a globally-unique ULID — so this table is neither partitioned nor
-- RLS-scoped on the lookup path: verification fetches the single row by primary key and then binds
-- the resulting grant to that row's `tenant_id`, which is the isolation. Only `SHA-256(secret)` is
-- stored (bytea, 32 bytes), never the secret. `scopes` is a text array of the wire names; an unknown
-- name written by a newer issuer is ignored on read (deny-by-default). `expires_at` is milliseconds
-- since the Unix epoch, matching the domain `Timestamp` exactly so no timezone rounding can move an
-- expiry boundary.
CREATE TABLE IF NOT EXISTS api_keys (
    id          text        PRIMARY KEY,
    tenant_id   text        NOT NULL,
    secret_hash bytea       NOT NULL,
    scopes      text[]      NOT NULL,
    revoked     boolean     NOT NULL DEFAULT false,
    expires_at  bigint,
    created_at  timestamptz NOT NULL DEFAULT now()
);
-- Listing a tenant's keys (the admin provisioning surface, a later slice) filters by tenant.
CREATE INDEX IF NOT EXISTS api_keys_tenant ON api_keys (tenant_id);
