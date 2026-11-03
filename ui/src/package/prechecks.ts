// The refusals the till decides itself before it paints a money act (ADR-0196 safeguard 2): for
// each act, the tokens of its catalogue entry's `refusals` (`./actions`) that the till can decide
// from what it holds. The catalogue lists what the edge refuses predictably; this lists the part of
// it the till checks. Everything else is the edge's to refuse, and the screen rolls back with the
// refusal's own sentence (safeguard 3). `unit/package-prechecks.test.ts` holds each token here to
// the act's catalogue entry.
//
// `WRITES_PENDING` is the till's own: a write to the bill still on its way (ADR-0196 safeguard 2,
// "no write still pending on the bill"), which the edge never sees as such.

import type { ActionId } from "./actions";

export const WRITES_PENDING = "WRITES_PENDING";

export const PRECHECKED: Readonly<Partial<Record<ActionId, readonly string[]>>> = {
  "billing.bill.settle": [WRITES_PENDING, "DRAWER_COUNTED", "PAYMENTS_DO_NOT_SUM_TO_TOTAL"],
  "billing.bill.void": [WRITES_PENDING, "APPROVAL_REQUIRED"],
  "billing.bill.void_settled": ["APPROVAL_REQUIRED"],
  "billing.bill.discount": [WRITES_PENDING, "APPROVAL_REQUIRED", "REDUCTION_EXCEEDS_BILL"],
  "billing.fee.waive": [WRITES_PENDING, "APPROVAL_REQUIRED", "FEE_NOT_WAIVABLE"],
};
