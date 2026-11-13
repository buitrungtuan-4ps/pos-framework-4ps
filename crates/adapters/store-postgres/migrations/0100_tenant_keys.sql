-- Copyright (c) 2026 Pizza 4P's. All rights reserved.
-- Proprietary and confidential. Internal use only. See LICENSE.
--
-- 0100 — tenant keys
-- ([ADR-0217](../../../../docs/adr/0217-secrets-are-sealed-under-versioned-tenant-keys.md)):
-- each tenant's secrets are sealed under its own random key, and the key is stored here only
-- wrapped by the environment key, which never reaches the database.
--
--   * `key_version`    — the tenant key's version, from 1; a sealed value names it.
--   * `keeper_version` — the environment-key version that wrapped it.
--   * `wrapped`        — the wrapped key: opaque bytes, never the key.
--   * `created_at_ms`  — when the cloud generated it, from the cloud's clock, in Unix milliseconds.
--
-- RETENTION: a version is kept while anything is sealed under it, and a tenant's versions are
-- deleted when its secrets are erased by key (ADR-0217 decision 4). No personal data.
--
-- Read and written by the cloud's own role only: row-level security is on and `app_tenant` is
-- granted nothing, so a tenant-scoped session cannot read another tenant's wrapped key, or its
-- own. Forward-only and additive, applied idempotently on every boot (ADR-0017).

CREATE TABLE IF NOT EXISTS tenant_keys (
    tenant_id      text    NOT NULL,
    key_version    integer NOT NULL CHECK (key_version > 0),
    keeper_version integer NOT NULL CHECK (keeper_version > 0),
    wrapped        bytea   NOT NULL,
    created_at_ms  bigint  NOT NULL,
    PRIMARY KEY (tenant_id, key_version)
);

ALTER TABLE tenant_keys ENABLE ROW LEVEL SECURITY;
