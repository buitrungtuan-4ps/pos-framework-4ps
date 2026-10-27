// Which cell this cloud is (ADR-0179 C1). Read-only: a cell changes in cloud.toml and by a deploy,
// never from the console. Typed from the generated OpenAPI schema (ADR-0177) rather than by hand.

import { requestJson } from "./client";
import type { components } from "./generated/admin";

/** The `[cell]` section as `GET /admin/cell` answers it. */
export type CellInfo = components["schemas"]["CellView"];

/** Reads which cell this cloud is. */
export const getCell = (): Promise<CellInfo> => requestJson<CellInfo>("GET", "/admin/cell");

/** A store's country against its cell's home country (ADR-0179 C6). */
export type CellStanding = components["schemas"]["CellStandingView"];
/** A recorded answer to a store's cell difference. */
export type CellAcknowledgement = components["schemas"]["CellAcknowledgementView"];

/** Reads whether a store's country differs from its cell's, and how that is answered. */
export const getCellStanding = (tenantId: string, storeId: string): Promise<CellStanding> =>
  requestJson<CellStanding>(
    "GET",
    `/admin/stores/${encodeURIComponent(storeId)}/cell-standing?tenant_id=${encodeURIComponent(tenantId)}`,
  );

/** Records why a store's cell difference is correct. Audited; never a refusal of the difference. */
export const acknowledgeCell = (
  tenantId: string,
  storeId: string,
  difference: { profile_country: string; cell_country: string; reason: string },
): Promise<CellAcknowledgement> =>
  requestJson<CellAcknowledgement>(
    "POST",
    `/admin/stores/${encodeURIComponent(storeId)}/cell-acknowledgement`,
    { tenant_id: tenantId, ...difference },
  );
