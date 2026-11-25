// `state/weigh.ts` (ADR-0223): a weighed item's line is told apart by its item, so the order screen
// shows its weight and offers no whole-unit stepper. Item names here are fake.

import { describe, expect, it } from "vitest";

import type { MenuItemResponse } from "../src/api/schemas";
import { weighedLine } from "../src/state/weigh";

const item = (id: string, soldByWeight: boolean) => ({ menu_item_id: id, sold_by_weight: soldByWeight }) as MenuItemResponse;
const items = new Map([["rye", item("rye", true)], ["tea", item("tea", false)]]);

describe("a weighed line", () => {
  it("is the line of an item sold by weight, and no other", () => {
    expect(weighedLine({ menuItemId: "rye" }, items)).toBe(true);
    expect(weighedLine({ menuItemId: "tea" }, items)).toBe(false);
    expect(weighedLine({ menuItemId: "gone" }, items)).toBe(false);
    expect(weighedLine({}, items)).toBe(false);
  });
});
