// `state/settle-act.ts`: the receipt line never makes up a receipt number (a fiscal fact), and a
// store that takes payment only against a fresh check refuses one before it is sent (ADR-0218).

import { afterEach, describe, expect, it } from "vitest";

import type { MenuResponse } from "../src/api/schemas";
import { receiptLine, settleRefusal } from "../src/state/settle-act";
import { applyMenu } from "../src/state/store";

/** The menu read of a store with nothing on it, saying whether a check comes before payment. */
function menu(checkBeforePayment: boolean): MenuResponse {
  return {
    items: [],
    modifier_groups: [],
    courses: [],
    currency: "VND",
    check_before_payment: checkBeforePayment,
  } as unknown as MenuResponse;
}

const cash = [
  {
    method: "PAYMENT_METHOD_CASH",
    tendered: { currency_code: "VND", amount_minor: 100_000 },
    applied_to_bill: { currency_code: "VND", amount_minor: 100_000 },
  },
];
const printed = (stale: boolean) => ({
  copy: 1,
  printed_time: "2026-11-18T08:00:00Z",
  stale,
  total_due: { currency_code: "VND", amount_minor: 100_000 },
});

afterEach(() => applyMenu(menu(false)));

describe("a check before payment", () => {
  it("is not asked for until the store asks for it", () => {
    applyMenu(menu(false));
    expect(settleRefusal(cash, 100_000)).toBeNull();
  });

  it("refuses a payment with no check, or a stale one, once the store asks for it", () => {
    applyMenu(menu(true));
    expect(settleRefusal(cash, 100_000)).toMatch(/check/i);
    expect(settleRefusal(cash, 100_000, printed(true))).toMatch(/check/i);
    expect(settleRefusal(cash, 100_000, printed(false))).toBeNull();
  });
});

describe("the settled panel's receipt line", () => {
  it("names the receipt the edge gave", () => {
    expect(receiptLine({ bill_id: "b", state: "BILL_STATE_SETTLED", print_receipt: true, receipt_number: 42 })).toContain(
      "42",
    );
  });

  it("says the number could not be read, never shows 0, when a re-read found no receipt", () => {
    const line = receiptLine({ bill_id: "b", state: "BILL_STATE_SETTLED", print_receipt: false });
    expect(line).toContain("could not be read");
    expect(line).not.toMatch(/#\s*0\b/);
  });
});
