// The stores this HQ reads in other cells (ADR-0179 C10). Read-only: `[federation]` changes in
// cloud.toml, never from the console. Only aggregates cross a cell boundary, and no key is ever
// answered. Typed from the generated OpenAPI schema (ADR-0177) rather than by hand.

import { requestJson } from "./client";
import type { components } from "./generated/admin";

/** `GET /admin/hq/rollups` as it answers. */
export type HqRollups = components["schemas"]["HqRollupsView"];

/** One store in another cell, with its rollups or why there are none. */
export type HqStore = components["schemas"]["HqStoreView"];

/** Reads the daily rollups of every store `[federation]` names. */
export const getHqRollups = (): Promise<HqRollups> =>
  requestJson<HqRollups>("GET", "/admin/hq/rollups");
