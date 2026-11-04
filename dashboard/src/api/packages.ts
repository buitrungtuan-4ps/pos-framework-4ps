// A tenant's packages and their published versions (ADR-0189), read to pin them. A pin is the
// `package` node's settings, written with `api.putSetting` in the order `lib/package-pin.ts` gives;
// publishing a version is the package builder's, not this console's yet. Typed from the generated
// OpenAPI schema (ADR-0177).

import { requestJson } from "./client";
import type { PackageRecord, PackageVersionRecord } from "./schemas";

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
