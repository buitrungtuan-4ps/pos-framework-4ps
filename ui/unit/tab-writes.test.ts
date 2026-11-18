// `state/tabs.ts` (ADR-0221, ADR-0196 decision 1): an empty tab closed from the list leaves it at
// once and comes back where it stood on a refusal; a label shows at once, a cleared one goes, and a
// refusal puts the old one back. Labels here are fake.

import { describe, expect, it, vi } from "vitest";

import { ApiError, api } from "../src/api/client";
import type { Tab } from "../src/api/schemas";
import { closeEmptyTab, labelTab, type TabList } from "../src/state/tabs";

function tabs(entries: readonly Partial<Tab>[]): TabList & { held: () => Tab[] } {
  let held = entries.map((entry, at) => ({ order_id: `o${at}`, tab_number: at + 1, ...entry }) as Tab);
  return {
    get: () => held,
    set: (next) => {
      held = next;
    },
    held: () => held,
  };
}

describe("an empty tab closed from the list", () => {
  it("leaves at once, and comes back where it stood on a refusal", async () => {
    const list = tabs([{}, {}, {}]);
    const refusal = new ApiError(409, "sold", "ORDER_NOT_EMPTY");
    const send = vi.spyOn(api, "closeOrder").mockImplementation(() => {
      expect(list.held().map((tab) => tab.order_id)).toEqual(["o0", "o2"]);
      return Promise.reject(refusal);
    });
    await expect(closeEmptyTab(list, "o1", "reason-1", vi.fn())).rejects.toBe(refusal);
    expect(list.held().map((tab) => tab.order_id)).toEqual(["o0", "o1", "o2"]);
    expect(send).toHaveBeenCalledWith("o1", "reason-1");
    send.mockRestore();
  });
});

describe("a tab's label", () => {
  it("shows at once, trimmed, and an empty one clears it", async () => {
    const list = tabs([{ label: "Fake A" }]);
    const send = vi.spyOn(api, "labelTab").mockResolvedValue({});
    await labelTab(list, "o0", "  Fake B ", vi.fn());
    expect(list.held()[0]?.label).toBe("Fake B");
    await labelTab(list, "o0", " ", vi.fn());
    expect(list.held()[0]?.label).toBeUndefined();
    expect(send).toHaveBeenLastCalledWith("o0", "");
    send.mockRestore();
  });

  it("puts the old label back on a refusal", async () => {
    const list = tabs([{ label: "Fake A" }]);
    const refusal = new ApiError(409, "off", "GUEST_LABEL_OFF");
    const send = vi.spyOn(api, "labelTab").mockRejectedValue(refusal);
    await expect(labelTab(list, "o0", "Fake B", vi.fn())).rejects.toBe(refusal);
    expect(list.held()[0]?.label).toBe("Fake A");
    send.mockRestore();
  });
});
