// A tenant's packages and their published versions (ADR-0189), read to pin them. A pin is the
// `package` node's settings, written with `api.putSetting` in the order `lib/package-pin.ts` gives;
// publishing a version is the package builder's, not this console's yet. Typed from the generated
// OpenAPI schema (ADR-0177).

import { requestJson } from "./client";
import type { PackageRecord, PackageVersionRecord, StorePackage } from "./schemas";

/** `GET /admin/packages`: the tenant's packages, in id order. */
export const listPackages = (tenantId: string): Promise<PackageRecord[]> =>
  requestJson<PackageRecord[]>(
    "GET",
    `/admin/packages?tenant_id=${encodeURIComponent(tenantId)}`,
  );

/** `GET /admin/packages/{package_id}/versions`: its published versions, in publish order. */
export const listPackageVersions = (
  tenantId: string,
  packageId: string,
): Promise<PackageVersionRecord[]> =>
  requestJson<PackageVersionRecord[]>(
    "GET",
    `/admin/packages/${encodeURIComponent(packageId)}/versions?tenant_id=${encodeURIComponent(tenantId)}`,
  );

/**
 * `PUT /admin/stores/{store_id}/package/choice`: the version a choosing store runs, one of those it
 * is allowed, or `null` to clear its choice so it runs its default. Written at the store and the
 * store republished; an HQ act behind `console.config.publish` (ADR-0189 decision 2).
 */
export const chooseStorePackage = (
  tenantId: string,
  storeId: string,
  packageVersionId: string | null,
): Promise<unknown> =>
  requestJson<unknown>("PUT", `/admin/stores/${encodeURIComponent(storeId)}/package/choice`, {
    tenant_id: tenantId,
    package_version_id: packageVersionId,
  });

/**
 * `GET /admin/stores/{store_id}/package`: the version a store is pinned to, the one it runs, and
 * why it waits when they differ (ADR-0189 decision 5).
 */
export const storePackage = (tenantId: string, storeId: string): Promise<StorePackage> =>
  requestJson<StorePackage>(
    "GET",
    `/admin/stores/${encodeURIComponent(storeId)}/package?tenant_id=${encodeURIComponent(tenantId)}`,
  );
