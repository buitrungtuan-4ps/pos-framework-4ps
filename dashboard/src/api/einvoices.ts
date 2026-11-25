// A store's e-invoices (ADR-0225, ADR-0240): the symbol it numbers them under. Typed from the
// generated OpenAPI schema (ADR-0177).

import { requestJson } from "./client";
import type { components } from "./generated/admin";

/** What a publish answers: the config version it made. */
export type PublishedConfigView = components["schemas"]["PublishedConfig"];

/**
 * Publishes `symbol` on the store's own `einvoice` node. The cloud checks it against the store's
 * country, keeps the node's settings and audits it. Not a setting, so not a layer publish.
 */
export const publishEInvoiceSymbol = (
  tenantId: string,
  storeId: string,
  symbol: string,
): Promise<PublishedConfigView> =>
  requestJson<PublishedConfigView>("PUT", "/admin/config/einvoice-symbol", {
    tenant_id: tenantId,
    store_id: storeId,
    symbol,
  });
