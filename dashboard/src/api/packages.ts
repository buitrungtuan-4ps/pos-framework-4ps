// A tenant's packages and their published versions (ADR-0189): created and published from the
// package designer, and read to pin them. A pin is the `package` node's settings, written with
// `api.putSetting` in the order `lib/package-pin.ts` gives. Typed from the generated OpenAPI schema
// (ADR-0177).

import { requestJson } from "./client";
import type {
  PackageRecord,
  PackageVersionDryRun,
  PackageVersionRecord,
  PackageVersionRequest,
  StorePackage,
} from "./schemas";

/** `GET /admin/packages`: the tenant's packages, in id order. */
export const listPackages = (tenantId: string): Promise<PackageRecord[]> =>
  requestJson<PackageRecord[]>(
    "GET",
    `/admin/packages?tenant_id=${encodeURIComponent(tenantId)}`,
  );

/** `POST /admin/packages`: a new package, its id minted by the server. */
export const createPackage = (tenantId: string, name: string): Promise<PackageRecord> =>
  requestJson<PackageRecord>("POST", "/admin/packages", { tenant_id: tenantId, name });

/**
 * `POST /admin/packages/{package_id}/versions/dry-run`: what a publish of `request` would find,
 * and where, with nothing stored (ADR-0190 decision 8).
 */
export const dryRunPackageVersion = (
  packageId: string,
  request: PackageVersionRequest,
): Promise<PackageVersionDryRun> =>
  requestJson<PackageVersionDryRun>(
    "POST",
    `/admin/packages/${encodeURIComponent(packageId)}/versions/dry-run`,
    request,
  );

/** `POST /admin/packages/{package_id}/versions`: publishes `request` as a version, for good. */
export const publishPackageVersion = (
  packageId: string,
  request: PackageVersionRequest,
): Promise<PackageVersionRecord> =>
  requestJson<PackageVersionRecord>(
    "POST",
    `/admin/packages/${encodeURIComponent(packageId)}/versions`,
    request,
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
