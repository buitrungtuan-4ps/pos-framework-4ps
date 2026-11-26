import { describe, expect, it } from "vitest";

import { noteLiveVendors, noteOrderVendors, orderVendor } from "../src/state/order-vendors";

// ADR-0248 decision 5: the vendor a delivery order came through is learned from the reads that list
// orders, and badges it until a whole read no longer lists it.
describe("order vendors", () => {
  it("adds from a partial read and replaces on a whole one", () => {
    noteOrderVendors([{ order_id: "o1", vendor_name: "Sandbox delivery" }, { order_id: "o2" }], false);
    expect(orderVendor("o1")).toBe("Sandbox delivery");
    expect(orderVendor("o2")).toBeUndefined();

    noteOrderVendors([{ order_id: "o3", vendor_name: "Other app" }], false);
    expect(orderVendor("o1")).toBe("Sandbox delivery");
    expect(orderVendor("o3")).toBe("Other app");

    noteLiveVendors({
      orders: [],
      kitchen: [{ order_id: "o3", vendor_name: "Other app" }] as never,
      opensBefore: 0,
    });
    expect(orderVendor("o1")).toBeUndefined();
    expect(orderVendor("o3")).toBe("Other app");
  });
});
