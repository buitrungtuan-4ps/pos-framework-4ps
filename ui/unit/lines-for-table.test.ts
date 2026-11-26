import { describe, expect, it } from "vitest";

import { PENDING, linesForTable, updateStore, type OrderLine } from "../src/state/store";

// A line added while its table's seat was on its way is painted under a provisional order. Once the
// seat lands and the till learns the table's order, the line is still the table's until the edge
// has it: read by the order alone it left the screen, and payment with it (replay flake, #1394).
describe("linesForTable", () => {
  it("keeps a line painted before the order was known once the order is known", () => {
    const line: OrderLine = {
      orderLineId: `${PENDING}1`,
      orderId: `${PENDING}order:T1`,
      name: "Iced tea",
      quantityMilli: 1000,
      lineTotal: { amount_minor: 39_500, currency_code: "VND" },
      state: "ORDER_LINE_STATE_ADDED",
      modifierMenuItemIds: [],
      notePresent: false,
      menuItemId: "item",
      pending: true,
    } as OrderLine;
    updateStore((draft) => {
      draft.lines[line.orderLineId] = line;
    });
    expect(linesForTable("T1").map((held) => held.orderLineId)).toEqual([`${PENDING}1`]);

    updateStore((draft) => {
      draft.tableOrder["T1"] = "01JORDER";
    });
    expect(linesForTable("T1").map((held) => held.orderLineId)).toEqual([`${PENDING}1`]);
    expect(linesForTable("T2")).toEqual([]);
  });
});
