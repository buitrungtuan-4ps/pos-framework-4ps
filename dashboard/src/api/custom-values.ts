// The custom-data report (ADR-0194 decision 7): how many of a store's objects hold each value of one
// non-personal field. Typed from the generated OpenAPI schema (ADR-0177).

import { requestJson } from "./client";
import type { ValueSummary } from "./schemas";

/** What a summary counts and groups by, and the filter it keeps to. */
export type SummaryRequest = {
  readonly object?: string;
  readonly objectTypeId?: string;
  readonly customFieldId: string;
  readonly filterFieldId?: string;
  readonly filterValue?: string;
};

/** `GET /admin/stores/{store_id}/custom-values/summary`, behind `console.data.read`. */
export const customValueSummary = (
  tenantId: string,
  storeId: string,
  request: SummaryRequest,
): Promise<ValueSummary> => {
  const query = new URLSearchParams({ tenant_id: tenantId, custom_field_id: request.customFieldId });
  if (request.object) {
    query.set("object", request.object);
  }
  if (request.objectTypeId) {
    query.set("object_type_id", request.objectTypeId);
  }
  if (request.filterFieldId && request.filterValue) {
    query.set("filter_field_id", request.filterFieldId);
    query.set("filter_value", request.filterValue);
  }
  return requestJson<ValueSummary>(
    "GET",
    `/admin/stores/${encodeURIComponent(storeId)}/custom-values/summary?${query.toString()}`,
  );
};
