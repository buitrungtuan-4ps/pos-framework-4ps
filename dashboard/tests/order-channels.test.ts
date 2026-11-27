// The console's editor for order types and sale sources (ADR-0260): its tokens, its compact value
// and its faults follow the register's rules. Fake configuration only.

import { describe, expect, it } from "vitest";

import { sourcesFault, toValue, tokenFrom, typesFault, withName } from "../src/lib/order-channels";

describe("the order channels the console edits", () => {
  it("makes a token from a name, Vietnamese included", () => {
    expect(tokenFrom("ORDER_TYPE_", "Tự đến lấy")).toBe("ORDER_TYPE_TU_DEN_LAY");
    expect(tokenFrom("SALE_SOURCE_", "Hotline 1900")).toBe("SALE_SOURCE_HOTLINE_1900");
  });

  it("writes an entry without its defaults", () => {
    expect(
      toValue([
        { order_type: "ORDER_TYPE_DINE_IN", base_order_type: "ORDER_TYPE_DINE_IN", active: true, default: false, names: {}, applies_to_sources: [] },
        { order_type: "ORDER_TYPE_PICKUP", base_order_type: "ORDER_TYPE_TAKEAWAY", active: false, names: withName({}, "en", "Pick-up") },
      ]),
    ).toEqual([
      { order_type: "ORDER_TYPE_DINE_IN", base_order_type: "ORDER_TYPE_DINE_IN" },
      { order_type: "ORDER_TYPE_PICKUP", base_order_type: "ORDER_TYPE_TAKEAWAY", active: false, names: { en: "Pick-up" } },
    ]);
    expect(withName({ en: "Pick-up", vi: "x" }, "vi", "  ")).toEqual({ en: "Pick-up" });
  });

  it("says what the cloud would refuse", () => {
    const pickup = { order_type: "ORDER_TYPE_PICKUP", base_order_type: "ORDER_TYPE_TAKEAWAY", names: { en: "Pick-up" } };
    expect(typesFault([pickup])).toBeNull();
    expect(typesFault([{ ...pickup, names: { vi: "Tự đến lấy" } }])).toEqual({ kind: "needs_english", token: "ORDER_TYPE_PICKUP" });
    expect(typesFault([pickup, pickup])).toEqual({ kind: "duplicate", token: "ORDER_TYPE_PICKUP" });
    expect(typesFault([{ ...pickup, order_type: "ORDER_TYPE_" }])).toEqual({ kind: "bad_token", token: "ORDER_TYPE_" });
    expect(typesFault([{ ...pickup, default: true, active: false }])).toEqual({ kind: "inactive_default", token: "ORDER_TYPE_PICKUP" });
    expect(typesFault([{ ...pickup, default: true }, { order_type: "ORDER_TYPE_DINE_IN", base_order_type: "ORDER_TYPE_DINE_IN", default: true }])).toEqual({ kind: "two_defaults" });
    expect(sourcesFault([{ source: "SALE_SOURCE_KIOSK" }])).toBeNull();
    expect(sourcesFault([{ source: "SALE_SOURCE_APP" }])).toEqual({ kind: "bad_token", token: "SALE_SOURCE_APP" });
    expect(sourcesFault([{ source: "SALE_SOURCE_HOTLINE", names: { en: "x".repeat(49) } }])).toEqual({ kind: "name_too_long", token: "SALE_SOURCE_HOTLINE" });
  });
});
