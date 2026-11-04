// `state/drawer-list-acts.ts` (ADR-0167, ADR-0204): how a re-read of the store's drawers says what
// came of starting, counting or closing a drawer from the list when the edge did not answer.

import { beforeEach, describe, expect, it, vi } from "vitest";

const vnd = (amount_minor: number) => ({ currency_code: "VND", amount_minor });
type Listed = { terminal_device_id: string; shift: { shift_id: string; state: string } | null; default_float: ReturnType<typeof vnd> };
let drawers: Listed[] = [];

vi.mock("../src/api/client", async (actual) => ({
  ...(await actual<typeof import("../src/api/client")>()),
  api: {
    drawers: async () => ({ drawer_model: "DRAWER_MODEL_PER_TERMINAL", drawers }),
    currentShift: async () => null,
  },
}));

const { recheckClosedDrawers, recheckCountedDrawer, recheckStarted } = await import("../src/state/drawer-list-acts");

const drawer = (till: string, shift: Listed["shift"]): Listed => ({ terminal_device_id: till, shift, default_float: vnd(0) });

beforeEach(() => {
  drawers = [];
});

describe("an act on the drawer list with no answer", () => {
  it("is a start made once the till's drawer has an open shift", async () => {
    drawers = [drawer("bar", null)];
    expect(await recheckStarted("bar", "Bar till")).toHaveProperty("refused");
    drawers = [drawer("bar", { shift_id: "s1", state: "SHIFT_STATE_OPEN" })];
    expect(await recheckStarted("bar", "Bar till")).toEqual({ committed: { started: "Bar till" } });
  });
  it("is a count made once that shift reads counted", async () => {
    drawers = [drawer("bar", { shift_id: "s1", state: "SHIFT_STATE_OPEN" })];
    expect(await recheckCountedDrawer("s1", "Bar till")).toHaveProperty("refused");
    drawers = [drawer("bar", { shift_id: "s1", state: "SHIFT_STATE_COUNTED" })];
    expect(await recheckCountedDrawer("s1", "Bar till")).toEqual({ committed: { counted: "Bar till" } });
  });
  it("is a close made only once none of its shifts is still listed", async () => {
    drawers = [drawer("bar", null), drawer("counter", { shift_id: "s2", state: "SHIFT_STATE_COUNTED" })];
    expect(await recheckClosedDrawers(["s1", "s2"])).toHaveProperty("refused");
    drawers = [drawer("bar", null), drawer("counter", null)];
    expect(await recheckClosedDrawers(["s1", "s2"])).toEqual({ committed: { closedRead: true } });
  });
});
