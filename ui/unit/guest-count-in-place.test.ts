// A seated table's missing guest count, given in place (ADR-0254): the order screen asks for it with
// one tap per count instead of "give the table back and seat it again"; a refusal of the edge's for
// a table this till did not seat teaches it the count is missing; another till's count reaches this
// one on the live link; and any other missing step is named by what its act does, never by its id.

import { afterEach, describe, expect, it, vi } from "vitest";
import { render } from "solid-js/web";

import { ApiError, api } from "../src/api/client";
import { FlowWait, GuestCountPrompt } from "../src/components/GuestCountPrompt";
import { adoptSession } from "../src/state/permissions";
import en from "../src/i18n/en.json";
import vi_ from "../src/i18n/vi.json";
import { errorMessage } from "../src/lib/errors";
import { ACTIONS } from "../../packages/package-renderer/src/actions";
import { applyPackage, flowWaitSentence, guestsOf, isGuestStep, learnMissingGuests, recordGuests, stepInWords } from "../src/state/flows";
import { fold, updateStore } from "../src/state/store";

/** A running version whose dine-in flow asks the count first, then a lookup before the bill. */
const RUNNING = {
  running: {
    flows: [
      {
        body: {
          flow_id: "dine_in",
          start_action_id: "sales.table.seat",
          steps: [
            { step_id: "guests", action_id: "sales.table.seat", kind: "STEP_KIND_REQUIRED" },
            { step_id: "loyalty_card", action_id: "integrations.lookup.run", kind: "STEP_KIND_REQUIRED" },
            { step_id: "bill", action_id: "billing.bill.open", kind: "STEP_KIND_REQUIRED", prerequisites: ["loyalty_card"] },
          ],
        },
      },
    ],
  },
};

function draw(tableId: string): { target: HTMLElement; dispose: () => void } {
  const target = document.createElement("div");
  document.body.append(target);
  const dispose = render(() => GuestCountPrompt({ tableId, sentence: flowWaitSentence("guests") }), target);
  return { target, dispose };
}

function countButton(target: HTMLElement, count: number): HTMLButtonElement {
  const found = [...target.querySelectorAll<HTMLButtonElement>('[data-step="countGuests"]')].find(
    (button) => button.textContent === String(count),
  );
  if (found === undefined) throw new Error(`no button for ${count}`);
  return found;
}

afterEach(() => {
  vi.restoreAllMocks();
  applyPackage(null);
  document.body.innerHTML = "";
});

describe("the guest count asked in place", () => {
  it("asks how many guests, and one tap records the count on the table's order", async () => {
    applyPackage(RUNNING);
    recordGuests("T1", undefined);
    const call = vi.spyOn(api, "countGuests").mockResolvedValue({ table_id: "T1", state: "TABLE_STATE_OCCUPIED" } as never);
    const { target, dispose } = draw("T1");
    expect(target.textContent).toContain("How many guests?");
    expect(target.textContent).not.toContain("Give the table back");
    countButton(target, 3).click();
    expect(guestsOf("T1")).toBe(3);
    await vi.waitFor(() => expect(call).toHaveBeenCalledWith("T1", 3));
    await vi.waitFor(() => expect(countButton(target, 3).disabled).toBe(false));
    expect(guestsOf("T1")).toBe(3);
    dispose();
  });

  it("puts back what the till knew and says the edge's refusal when the count is refused", async () => {
    applyPackage(RUNNING);
    recordGuests("T2", undefined);
    vi.spyOn(api, "countGuests").mockRejectedValue(new ApiError(409, "no", "PERMISSION_DENIED"));
    const { target, dispose } = draw("T2");
    countButton(target, 2).click();
    await vi.waitFor(() => expect(target.querySelector('[data-outcome="guests-refused"]')).not.toBeNull());
    expect(guestsOf("T2")).toBe("skipped");
    dispose();
  });

  it("asks only a person who may seat tables; anyone else reads the sentence", () => {
    applyPackage(RUNNING);
    const shown = () => {
      const target = document.createElement("div");
      document.body.append(target);
      const dispose = render(() => FlowWait({ tableId: "T5", step: "guests" }), target);
      const outcome = target.firstElementChild?.getAttribute("data-outcome");
      dispose();
      return outcome;
    };
    adoptSession({ permissions_enforced: true, permissions: ["sales.line.add", "sales.table.manage"] } as never);
    expect(shown()).toBe("guests-asked");
    adoptSession({ permissions_enforced: true, permissions: ["sales.line.add"] } as never);
    expect(shown()).toBe("flow-waits");
    adoptSession({ permissions_enforced: false } as never);
  });

  it("learns from the edge's refusal that a table it did not seat has no count", () => {
    applyPackage(RUNNING);
    expect(guestsOf("T3")).toBeUndefined();
    learnMissingGuests("T3", new ApiError(409, "step loyalty_card of flow dine_in comes first", "FLOW_PREREQUISITE_UNMET"));
    expect(guestsOf("T3")).toBeUndefined();
    learnMissingGuests("T3", new ApiError(409, "step guests of flow dine_in comes first", "FLOW_PREREQUISITE_UNMET"));
    expect(guestsOf("T3")).toBe("skipped");
  });

  it("takes another till's count from the live link: the latest wins", () => {
    updateStore((draft) => {
      draft.orderTable["O4"] = "T4";
    });
    recordGuests("T4", 2);
    fold({ eventType: "sales.order.guests_counted", payload: { order_id: "O4", guest_count: 6 } } as never);
    expect(guestsOf("T4")).toBe(6);
  });
});

describe("a missing step in words", () => {
  it("names a step by what its act does, never by its id", () => {
    applyPackage(RUNNING);
    expect(isGuestStep("guests")).toBe(true);
    expect(isGuestStep("loyalty_card")).toBe(false);
    expect(flowWaitSentence("loyalty_card")).toBe("The store asks for a step first: look something up.");
    expect(stepInWords("guests")).toBe("the guest count");
    expect(stepInWords("no_such_step")).toBe("another step of the store's flow");
    const refusal = new ApiError(409, "step loyalty_card of flow dine_in comes first", "FLOW_PREREQUISITE_UNMET");
    expect(errorMessage(refusal)).not.toContain("loyalty_card");
    expect(errorMessage(refusal)).toContain("look something up");
  });

  it.each([
    ["en", en as Record<string, string>],
    ["vi", vi_ as Record<string, string>],
  ])("has words in %s for every act a step can run", (_lang, catalogue) => {
    const unnamed = ACTIONS.map((action) => action.action_id as string).filter(
      (id) => !catalogue[`flow.act.${id.replaceAll(".", "_")}`],
    );
    expect(unnamed).toEqual([]);
  });
});
