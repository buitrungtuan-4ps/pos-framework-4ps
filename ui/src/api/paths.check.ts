// The till's route types (ADR-0177 decision 3): `tsc` refuses a call the document does not
// declare. Each `@ts-expect-error` line is the test — if one stops being an error, `tsc` fails on the
// unused directive. Nothing imports this file, so it never reaches the bundle.

import type { EdgeCall, EdgePath, EdgeUrl, EdgeUrlFor } from "./paths";

// A declared route, filled in, with or without a query.
export const accepted: readonly [EdgePath, EdgeUrl<"/api/bills/{id}/check">, EdgeUrlFor<"GET">, EdgeCall] = [
  "/api/bills/{id}/check",
  `/api/bills/${"b1"}/check`,
  `/api/menu?channel=${"SALES_CHANNEL_TAKEAWAY"}`,
  ["POST", "/api/session/sign-out"],
];

// @ts-expect-error -- no such route
export const typo: EdgeUrlFor<"GET"> = "/api/flor";

// @ts-expect-error -- the till's routes only
export const elsewhere: EdgeUrlFor<"GET"> = "/admin/fees";

// @ts-expect-error -- a route name, not a URL: no placeholders filled in
export const unfilled: EdgePath = "/api/bills/b1/check";

// @ts-expect-error -- `/api/session/sign-out` takes POST only
export const wrongMethod: EdgeUrlFor<"GET"> = "/api/session/sign-out";

// @ts-expect-error -- the tuple pairs the method with the route
export const wrongPair: EdgeCall = ["DELETE", "/api/session/sign-out"];
