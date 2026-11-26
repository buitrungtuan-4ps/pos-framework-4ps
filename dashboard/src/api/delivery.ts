// A store's delivery reconciliation (ADR-0248 decision 3): each day's vendors, the orders they
// sent, what they took gross, the commission at the vendor's rate and the net. Typed from the
// generated OpenAPI schema (ADR-0177).

import { requestJson } from "./client";
import type { components } from "./generated/admin";

/** One vendor's orders on one trading day. */
export type DeliveryReconciliationRow = components["schemas"]["DeliveryReconciliationRow"];

/** The store's delivery reconciliation over `window`, oldest day first. T2: revenue holders only. */
export const deliveryReconciliation = (
  tenantId: string,
  storeId: string,
  window: { from?: string; to?: string },
): Promise<DeliveryReconciliationRow[]> => {
  const params = new URLSearchParams({ tenant_id: tenantId });
  if (window.from) {
    params.set("from", window.from);
  }
  if (window.to) {
    params.set("to", window.to);
  }
  return requestJson<DeliveryReconciliationRow[]>(
    "GET",
    `/admin/stores/${encodeURIComponent(storeId)}/delivery/reconciliation?${params.toString()}`,
  );
};
