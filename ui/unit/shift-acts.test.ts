// `state/shift-acts.ts` (ADR-0196, ADR-0204): what the till refuses about opening or counting its
// drawer before sending, and how a re-read of the drawer's shift says what came of an act with no
// answer, with the edge's figures rather than the till's.

import { beforeEach, describe, expect, it, vi } from "vitest";

let current: { shift_id: string; state: string; counted_amount?: { currency_code: string; amount_minor: number } } | null =
  null;

vi.mock("../src/api/client", async (actual) => ({
  ...(await actual<typeof import("../src/api/client")>()),
  api: { currentShift: async () => current },
}));

const { addsUp, countRefusal, openRefusal, recheckClose, recheckCount, recheckOpen } = await import(
  "../src/state/shift-acts"
);

const vnd = (amount_minor: number) => ({ currency_code: "VND", amount_minor });
const notes = (count: number, uncounted?: number) => ({
  denomination_counts: [{ denomination: vnd(50_000), quantity: count }],
  uncounted_amount: uncounted === undefined ? undefined : vnd(uncounted),
});

beforeEach(() => {
  current = null;
});

describe("a count note by note", () => {
  it("adds up when its notes and loose amount come to the total, or it has no notes", () => {
    expect(addsUp(100_000, notes(2))).toBe(true);
    expect(addsUp(120_000, notes(2, 20_000))).toBe(true);
    expect(addsUp(100_000, undefined)).toBe(true);
    expect(addsUp(150_000, notes(2))).toBe(false);
  });
});

describe("opening and counting the drawer, before they are sent", () => {
  it("refuses an open while the till holds a shift not yet closed", () => {
    expect(openRefusal({ shiftId: "s", state: "SHIFT_STATE_OPEN" }, 100_000)).not.toBeNull();
    expect(openRefusal({ shiftId: "s", state: "SHIFT_STATE_COUNTED" }, 100_000)).not.toBeNull();
    expect(openRefusal({ shiftId: "s", state: "SHIFT_STATE_CLOSED" }, 100_000)).toBeNull();
    expect(openRefusal(null, 100_000)).toBeNull();
  });
  it("refuses a float or a count whose notes do not come to it", () => {
    expect(openRefusal(null, 150_000, notes(2))).not.toBeNull();
    expect(countRefusal(150_000, notes(2))).not.toBeNull();
    expect(countRefusal(100_000, notes(2))).toBeNull();
  });
});

describe("an act on the drawer with no answer", () => {
  it("is an open made once the drawer has an open shift", async () => {
    expect(await recheckOpen()).toHaveProperty("refused");
    current = { shift_id: "s1", state: "SHIFT_STATE_OPEN" };
    expect(await recheckOpen()).toEqual({ committed: current });
  });
  it("is a count made once the shift reads counted, with the count the edge recorded", async () => {
    current = { shift_id: "s1", state: "SHIFT_STATE_OPEN" };
    expect(await recheckCount("s1")).toHaveProperty("refused");
    current = { shift_id: "s1", state: "SHIFT_STATE_COUNTED", counted_amount: vnd(90_000) };
    expect(await recheckCount("s1")).toEqual({ committed: current });
    current = null;
    expect(await recheckCount("s1")).toHaveProperty("refused");
  });
  it("is a close made once the drawer no longer has that shift open", async () => {
    current = { shift_id: "s1", state: "SHIFT_STATE_COUNTED" };
    expect(await recheckClose("s1")).toHaveProperty("refused");
    current = null;
    expect(await recheckClose("s1")).toEqual({ committed: { read: true } });
  });
});
