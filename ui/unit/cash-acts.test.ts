// `state/cash-acts.ts` (ADR-0196, ADR-0204): what the till refuses about cash in or out of the
// drawer before sending it, and how a re-read of the drawer's shift says whether a movement with no
// answer was made: by the shift's total for that way, which the edge keeps.

import { beforeEach, describe, expect, it, vi } from "vitest";

const vnd = (amount_minor: number) => ({ currency_code: "VND", amount_minor });
const shift = (state: string, paidOut = 0, dropped = 0) => ({
  shift_id: "s1",
  state,
  opening_float: vnd(100_000),
  paid_in_amount: vnd(0),
  paid_out_amount: vnd(paidOut),
  dropped_amount: vnd(dropped),
  print_shift_report: false,
});
let current: ReturnType<typeof shift> | null = null;

vi.mock("../src/api/client", async (actual) => ({
  ...(await actual<typeof import("../src/api/client")>()),
  api: { currentShift: async () => current },
}));

const { movementRefusal, recheckMovement } = await import("../src/state/cash-acts");
const { applyShift } = await import("../src/state/store");

beforeEach(() => {
  current = null;
  applyShift(null);
});

describe("cash in or out of the drawer, before it is sent", () => {
  it("is refused with no open shift, and goes with one", () => {
    expect(movementRefusal("paid_out", 20_000)).not.toBeNull();
    applyShift(shift("SHIFT_STATE_COUNTED"));
    expect(movementRefusal("paid_out", 20_000)).not.toBeNull();
    applyShift(shift("SHIFT_STATE_OPEN"));
    expect(movementRefusal("paid_out", 20_000)).toBeNull();
  });
  it("refuses a drop whose notes do not come to it", () => {
    applyShift(shift("SHIFT_STATE_OPEN"));
    const notes = { denomination_counts: [{ denomination: vnd(50_000), quantity: 2 }] };
    expect(movementRefusal("safe_drop", 150_000, notes)).not.toBeNull();
    expect(movementRefusal("safe_drop", 100_000, notes)).toBeNull();
  });
});

describe("a movement with no answer", () => {
  it("is made once the shift's total for its way grew by the amount", async () => {
    current = shift("SHIFT_STATE_OPEN", 20_000);
    expect(await recheckMovement("s1", { kind: "paid_out", minor: 20_000, before: 0 })).toEqual({
      committed: { kind: "paid_out", minor: 20_000, drawer: null },
    });
  });
  it("is not made while the total is as it was, or when it was never sent", async () => {
    current = shift("SHIFT_STATE_OPEN", 0, 0);
    expect(await recheckMovement("s1", { kind: "safe_drop", minor: 50_000, before: 0 })).toHaveProperty("refused");
    current = shift("SHIFT_STATE_OPEN", 20_000);
    expect(await recheckMovement("s1", { kind: "paid_out", minor: 20_000, before: null })).toHaveProperty("refused");
  });
  it("is not called made when the shift has gone", async () => {
    expect(await recheckMovement("s1", { kind: "paid_in", minor: 10_000, before: 0 })).toHaveProperty("refused");
  });
});
