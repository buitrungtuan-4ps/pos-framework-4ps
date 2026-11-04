// `state/draft-walk-in.ts` (UX-16, ADR-0204 decision 5): a draft walk-in opens its order on the edge
// at the first item. A refused open opened nothing, so the next tap opens again. An open with no
// answer may have opened one the till cannot tell apart, so the draft opens nothing more and says
// the order is not confirmed.

import { afterEach, describe, expect, it, vi } from "vitest";

import { api, ApiError } from "../src/api/client";
import { t } from "../src/i18n";
import { errorMessage } from "../src/lib/errors";
import { DRAFT, draftWalkIn, keyToAddTo } from "../src/state/draft-walk-in";

const OPENED = { order_id: "01JDRAFTORDER0000000000001", business_date: "2026-11-04", queue_number: 7 };

function opens(...outcomes: (Error | null)[]) {
  const sent = vi.spyOn(api, "openOrder");
  for (const outcome of outcomes) {
    sent.mockImplementationOnce(() => (outcome === null ? Promise.resolve(OPENED) : Promise.reject(outcome)));
  }
  vi.spyOn(api, "liveOrders").mockResolvedValue([]);
  vi.spyOn(api, "kitchenOrders").mockResolvedValue([]);
  return sent;
}

async function tap(): Promise<unknown> {
  try {
    return await keyToAddTo(DRAFT, "draft", () => undefined);
  } catch (caught) {
    return caught;
  }
}

afterEach(() => {
  vi.restoreAllMocks();
});

describe("a draft walk-in's first open", () => {
  it("refused, lets the next tap open it again", async () => {
    draftWalkIn();
    const refusal = new ApiError(409, "no shift", "OPEN_SHIFT_REQUIRED");
    const sent = opens(refusal, null);
    expect(await tap()).toBe(refusal);
    const moved: string[] = [];
    await keyToAddTo(DRAFT, "draft", (path) => moved.push(path));
    expect(sent).toHaveBeenCalledTimes(2);
    expect(moved).toEqual([`/order/${OPENED.order_id}`]);
  });

  it("unanswered, opens nothing more and says the order is not confirmed", async () => {
    draftWalkIn();
    const sent = opens(new TypeError("network"), null);
    const first = await tap();
    const second = await tap();
    expect(sent).toHaveBeenCalledTimes(1);
    expect(errorMessage(first)).toBe(t("order.draft_unconfirmed"));
    expect(errorMessage(second)).toBe(t("order.draft_unconfirmed"));
  });

  it("unanswered, opens again only on a new draft", async () => {
    draftWalkIn();
    const sent = opens(new TypeError("network"), null);
    await tap();
    draftWalkIn();
    await tap();
    expect(sent).toHaveBeenCalledTimes(2);
  });
});
