// A weighed item (ADR-0223) asks for its weight rather than adding one, and its weight shows in
// kilograms to the gram.

import { describe, expect, it } from "vitest";

import type { MenuItemResponse } from "../src/api/schemas";
import { askWeight, kilograms, stopWeighing, weighingItem } from "../src/state/weigh";

const item = (soldByWeight: boolean) => ({ menu_item_id: "01J0000000000000000000000A", sold_by_weight: soldByWeight }) as MenuItemResponse;

describe("a weighed item", () => {
  it("is asked its weight, and an item sold by the piece is not", () => {
    expect(askWeight(item(false))).toBe(false);
    expect(weighingItem()).toBeNull();
    expect(askWeight(item(true))).toBe(true);
    expect(weighingItem()?.sold_by_weight).toBe(true);
    stopWeighing();
    expect(weighingItem()).toBeNull();
  });

  it("shows its grams in kilograms", () => {
    expect(kilograms(375)).toBe("0.375 kg");
    expect(kilograms(1205)).toBe("1.205 kg");
    expect(kilograms(5)).toBe("0.005 kg");
  });
});
