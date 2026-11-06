// `state/reactions.ts` (ADR-0193 decision 5): a trigger runs the reactions for it, at most 16, after
// the frame; only an act the signed-in person holds outright; never one whose condition the till
// cannot answer; and an act a reaction runs fires no reaction of its own.

import { afterEach, describe, expect, it, vi } from "vitest";

import { api } from "../src/api/client";
import { adoptSession } from "../src/state/permissions";
import { applyReactions, react, reactionsFor, setScreenOpener, type Fired } from "../src/state/reactions";

const ON_SEAT: Fired = { tableId: "T1", orderId: "O1", channel: "SALES_CHANNEL_DINE_IN", facts: {} };

function running(...reactions: object[]): void {
  applyReactions({ running: { reactions: reactions.map((body) => ({ body })) } });
}

function reaction(id: string, actionId: string, extra: object = {}): object {
  return { reaction_id: id, when: "REACTION_TRIGGER_ORDER_OPENED", then: { action_id: actionId }, ...extra };
}

afterEach(() => {
  vi.restoreAllMocks();
  applyReactions({ running: null });
});

describe("a reaction", () => {
  it("runs at most sixteen per trigger, its own trigger's only", () => {
    running(
      ...Array.from({ length: 18 }, (_, n) => reaction(`r${n}`, "billing.check.print")),
      { ...reaction("bill", "billing.check.print"), when: "REACTION_TRIGGER_BILL_OPENED" },
    );
    expect(reactionsFor("REACTION_TRIGGER_ORDER_OPENED", ON_SEAT)).toHaveLength(16);
    expect(reactionsFor("REACTION_TRIGGER_BILL_OPENED", ON_SEAT).map((each) => each.reaction_id)).toEqual(["bill"]);
  });

  it("runs after the frame, for the subject, only what the person holds outright", async () => {
    adoptSession({ permissions_enforced: true, permissions: ["billing.bill.open"] } as never);
    const check = vi.spyOn(api, "printOrderCheck").mockResolvedValue({} as never);
    const x = vi.spyOn(api, "printX").mockResolvedValue({} as never);
    running(reaction("check", "billing.check.print"), reaction("x", "reports.shift.print_x"));
    react("REACTION_TRIGGER_ORDER_OPENED", ON_SEAT);
    expect(check).not.toHaveBeenCalled();
    await vi.waitFor(() => expect(check).toHaveBeenCalledWith("O1"));
    expect(x).not.toHaveBeenCalled();
  });

  it("does not run when its condition cannot be answered", async () => {
    adoptSession({ permissions_enforced: false } as never);
    const check = vi.spyOn(api, "printOrderCheck").mockResolvedValue({} as never);
    const screens: string[] = [];
    setScreenOpener((screenId) => screens.push(screenId) > 0);
    running(
      reaction("check", "billing.check.print", { if: "order.line_count >= 1" }),
      reaction("away", "ui.screen.open", { then: { action_id: "ui.screen.open", params: { screen_id: "floor" } } }),
    );
    react("REACTION_TRIGGER_ORDER_OPENED", ON_SEAT);
    await vi.waitFor(() => expect(screens).toEqual(["floor"]));
    expect(check).not.toHaveBeenCalled();
  });

  it("does not answer the screen another reaction opened", async () => {
    adoptSession({ permissions_enforced: false } as never);
    const screens: string[] = [];
    setScreenOpener((screenId) => screens.push(screenId) > 0);
    const opens = { then: { action_id: "ui.screen.open", params: { screen_id: "order" } } };
    running(
      reaction("open", "ui.screen.open", opens),
      { reaction_id: "again", when: "REACTION_TRIGGER_SCREEN_ENTERED", screen_id: "order", ...opens },
    );
    react("REACTION_TRIGGER_ORDER_OPENED", ON_SEAT);
    await vi.waitFor(() => expect(screens).toEqual(["order"]));
    react("REACTION_TRIGGER_SCREEN_ENTERED", { ...ON_SEAT, screenId: "order" });
    await new Promise((resolve) => setTimeout(resolve, 120));
    expect(screens).toEqual(["order"]);
  });
});
