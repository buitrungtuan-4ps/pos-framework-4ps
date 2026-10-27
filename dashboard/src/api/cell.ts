// Which cell this cloud is (ADR-0179 C1). Read-only: a cell changes in cloud.toml and by a deploy,
// never from the console. Typed from the generated OpenAPI schema (ADR-0177) rather than by hand.

import { requestJson } from "./client";
import type { components } from "./generated/admin";

/** The `[cell]` section as `GET /admin/cell` answers it. */
export type CellInfo = components["schemas"]["CellView"];

/** Reads which cell this cloud is. */
export const getCell = (): Promise<CellInfo> => requestJson<CellInfo>("GET", "/admin/cell");
