-- Copyright (c) 2026 Pizza 4P's. All rights reserved.
-- Proprietary and confidential. Internal use only. See LICENSE.

-- A tenant's customization packages and their published versions
-- ([ADR-0189](../../../../docs/adr/0189-a-store-runs-a-package-version-hq-pins.md) decision 1).
--
-- `packages` holds one row per package. `package_versions` holds one row per published version:
-- `doc` is the version's content exactly as it rides a store's `package` node. A published version
-- never changes, so `app_tenant` is granted SELECT and INSERT only, with no UPDATE and no DELETE.
-- The trusted pool owner bypasses RLS, so this grant is the second line, behind the store's own
-- insert-only seam.
--
-- Tenant-scoped like the catalog: RLS on `app.tenant_id`. `package_id` on a version names a
-- `packages` row, which the application enforces (the no-FK posture every cloud table keeps).
-- Forward-only and additive, applied idempotently on every boot (ADR-0017). Greenfield: no backfill.

CREATE TABLE IF NOT EXISTS packages (
    tenant_id   text        NOT NULL,
    package_id  text        NOT NULL,
    doc         jsonb       NOT NULL,
    create_time timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (tenant_id, package_id)
);

ALTER TABLE packages ENABLE ROW LEVEL SECURITY;
DROP POLICY IF EXISTS packages_tenant_isolation ON packages;
CREATE POLICY packages_tenant_isolation ON packages
    USING (tenant_id = current_setting('app.tenant_id', true));
GRANT SELECT, INSERT ON packages TO app_tenant;

CREATE TABLE IF NOT EXISTS package_versions (
    tenant_id          text        NOT NULL,
    package_version_id text        NOT NULL,
    package_id         text        NOT NULL,
    doc                jsonb       NOT NULL,
    create_time        timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (tenant_id, package_version_id)
);
CREATE INDEX IF NOT EXISTS package_versions_by_package
    ON package_versions (tenant_id, package_id, package_version_id);

ALTER TABLE package_versions ENABLE ROW LEVEL SECURITY;
DROP POLICY IF EXISTS package_versions_tenant_isolation ON package_versions;
CREATE POLICY package_versions_tenant_isolation ON package_versions
    USING (tenant_id = current_setting('app.tenant_id', true));
GRANT SELECT, INSERT ON package_versions TO app_tenant;
