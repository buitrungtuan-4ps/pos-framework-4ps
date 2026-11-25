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

/** What an allocation answers: the symbol given and the config version that published it. */
export type AllocatedSymbolView = components["schemas"]["AllocatedEInvoiceSymbol"];

/**
 * Gives the store the next free symbol of its country's rule (ADR-0240 decision 3). The cloud
 * records it in the tenant's ledger, publishes it and audits it.
 */
export const allocateEInvoiceSymbol = (
  tenantId: string,
  storeId: string,
): Promise<AllocatedSymbolView> =>
  requestJson<AllocatedSymbolView>("POST", "/admin/config/einvoice-symbol/allocate", {
    tenant_id: tenantId,
    store_id: storeId,
  });

/** One invoice and where its submission stands (ADR-0241). No buyer is named. */
export type EInvoiceView = components["schemas"]["EInvoiceView"];

/** The tenant's invoices, or one store's, newest first. */
export const listEInvoices = (tenantId: string, storeId: string): Promise<EInvoiceView[]> =>
  requestJson<EInvoiceView[]>(
    "GET",
    `/admin/einvoices?tenant_id=${encodeURIComponent(tenantId)}&store_id=${encodeURIComponent(storeId)}`,
  );

/** Records an adjustment or a replacement of an issued invoice (ADR-0225 decision 7). */
export const correctEInvoice = (
  tenantId: string,
  invoiceId: string,
  kind: "ADJUSTMENT" | "REPLACEMENT",
  totalMinor: number,
): Promise<EInvoiceView> =>
  requestJson<EInvoiceView>("POST", `/admin/einvoices/${encodeURIComponent(invoiceId)}/correct`, {
    tenant_id: tenantId,
    kind,
    total_minor: totalMinor,
  });
