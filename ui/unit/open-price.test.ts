// `state/sell.ts` (ADR-0222 decision 4): an open-price item asks its price before it sells; the
// typed price rides that item's next line once; a fixed-price item is never asked.

import { describe, expect, it, vi } from "vitest";

import type { MenuItemResponse } from "../src/api/schemas";
import { money } from "../src/lib/money";
import { askOpenPrice, cancelPricing, priceTyped, pricingItem, takeTypedPrice } from "../src/state/sell";

const fixed = { menu_item_id: "bread" } as MenuItemResponse;
const open = {
  menu_item_id: "cake",
  open_price: { minimum: money("VND", 10_000), maximum: money("VND", 90_000) },
} as MenuItemResponse;

describe("an open-price item", () => {
  it("asks its price, sells once it is typed, and the price rides one line", () => {
    const sell = vi.fn();
    expect(askOpenPrice(fixed, sell)).toBe(false);
    expect(askOpenPrice(open, sell)).toBe(true);
    expect(pricingItem()).toBe(open);
    priceTyped(money("VND", 45_000));
    expect(sell).toHaveBeenCalledWith(open);
    expect(askOpenPrice(open, sell)).toBe(false);
    expect(takeTypedPrice("bread")).toBeUndefined();
    expect(takeTypedPrice("cake")).toEqual(money("VND", 45_000));
    expect(takeTypedPrice("cake")).toBeUndefined();
  });

  it("sells nothing when the price is cancelled", () => {
    const sell = vi.fn();
    askOpenPrice(open, sell);
    cancelPricing();
    expect(pricingItem()).toBeNull();
    expect(sell).not.toHaveBeenCalled();
  });
});
