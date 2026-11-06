// A till that reloads reads each table's guest count with what is open (`guest_count` on
// `GET /api/orders/live`), as it hears it from a seat on the live link (`state/flows.ts`).

import { describe, expect, it } from "vitest";

import { guestsOf } from "../src/state/flows";
import { applyLiveOrders } from "../src/state/store";

const order = (orderId: string, tableId: string, guestCount?: number) => ({
  order_id: orderId,
  table_id: tableId,
  open_bill_ids: [],
  lines: [],
  ...(guestCount === undefined ? {} : { guest_count: guestCount }),
});

describe("the live read", () => {
  it("records each table's count, and a table seated without one as not given", () => {
    applyLiveOrders({
      orders: [order("O1", "T1", 4), order("O2", "T2")] as never,
      kitchen: null,
      opensBefore: 0,
    });
    expect(guestsOf("T1")).toBe(4);
    expect(guestsOf("T2")).toBe("skipped");
    expect(guestsOf("T3")).toBeUndefined();
  });
});
