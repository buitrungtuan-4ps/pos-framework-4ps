// `state/unsent.ts` with `state/pay-first.ts` (ADR-0222 decision 6): on a store that pays first, a
// counter order's unsent lines are sent by its settle, so payment is not held to ask; a table's
// order, which a pay-first store never has, and every order on any other store still ask.

import { afterEach, describe, expect, it } from "vitest";

import type { MenuResponse } from "../src/api/schemas";
import { applyPayFirst } from "../src/state/pay-first";
import { type OrderLine, updateStore, walkInKey } from "../src/state/store";
import { createSendFirst } from "../src/state/unsent";

function unsentLineOn(key: string, orderId: string): void {
  updateStore((draft) => {
    draft.tableOrder[key] = orderId;
    draft.lines[`line-${orderId}`] = {
      orderLineId: `line-${orderId}`,
      orderId,
      state: "ORDER_LINE_STATE_ADDED",
    } as OrderLine;
  });
}

afterEach(() => applyPayFirst({ pay_first_enabled: false } as MenuResponse));

describe("taking payment for unsent lines", () => {
  it("is not held to ask on a pay-first store's counter order", () => {
    applyPayFirst({ pay_first_enabled: true } as MenuResponse);
    unsentLineOn(walkInKey("order-a"), "order-a");
    unsentLineOn("table-1", "order-b");
    const gate = createSendFirst();
    expect(gate.ask(walkInKey("order-a"), () => undefined)).toBe(false);
    expect(gate.ask("table-1", () => undefined)).toBe(true);
  });

  it("asks first everywhere else", () => {
    unsentLineOn(walkInKey("order-c"), "order-c");
    expect(createSendFirst().ask(walkInKey("order-c"), () => undefined)).toBe(true);
  });
});
