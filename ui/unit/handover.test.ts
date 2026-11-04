// `state/handover.ts` (ADR-0187, ADR-0204): how a re-read of the drawer's shift says what came of a
// handover's outgoing close or incoming opening when the edge did not answer, on the edge's figures.

import { beforeEach, describe, expect, it, vi } from "vitest";

const vnd = (amount_minor: number) => ({ currency_code: "VND", amount_minor });
type Current = { shift_id: string; state: string; handed_over_from_shift_id?: string } | null;
let current: Current = null;

vi.mock("../src/api/client", async (actual) => ({
  ...(await actual<typeof import("../src/api/client")>()),
  api: { currentShift: async () => current },
}));

const { recheckIn, recheckOut } = await import("../src/state/handover");
const { applyShift } = await import("../src/state/store");

const held = (state: string, counted?: number) => ({
  shift_id: "s1",
  state,
  opening_float: vnd(100_000),
  paid_in_amount: vnd(0),
  paid_out_amount: vnd(0),
  print_shift_report: false,
  counted_amount: counted === undefined ? undefined : vnd(counted),
});

beforeEach(() => {
  current = null;
  applyShift(null);
});

describe("a handover's outgoing act with no answer", () => {
  it("is not made while the shift is still open or still counted", async () => {
    current = { shift_id: "s1", state: "SHIFT_STATE_OPEN" };
    expect(await recheckOut("s1")).toHaveProperty("refused");
    current = { shift_id: "s1", state: "SHIFT_STATE_COUNTED" };
    expect(await recheckOut("s1")).toHaveProperty("refused");
  });
  it("is made once the shift is gone, on the count the edge recorded, and waits for that count", async () => {
    applyShift(held("SHIFT_STATE_COUNTED"));
    expect(await recheckOut("s1")).toBeNull();
    applyShift(held("SHIFT_STATE_CLOSED", 90_000));
    expect(await recheckOut("s1")).toEqual({ committed: { closed: { shiftId: "s1", counted: vnd(90_000) } } });
  });
});

describe("a handover's incoming opening with no answer", () => {
  it("is made once the drawer has the shift that took the outgoing one over", async () => {
    expect(await recheckIn("s1")).toHaveProperty("refused");
    current = { shift_id: "s2", state: "SHIFT_STATE_OPEN" };
    expect(await recheckIn("s1")).toHaveProperty("refused");
    current = { shift_id: "s2", state: "SHIFT_STATE_OPEN", handed_over_from_shift_id: "s1" };
    expect(await recheckIn("s1")).toEqual({ committed: current });
  });
});
