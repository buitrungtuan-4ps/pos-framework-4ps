// `state/take-act.ts` and the even split's guest counts (ADR-0220): a share is refused before it is
// sent when it is more than the bill still owes, and a store that takes a bill in one payment is
// offered no even split, since every share but the last is a payment of its own.

import { afterEach, describe, expect, it } from "vitest";

import type { MenuResponse } from "../src/api/schemas";
import { takeRefusal } from "../src/state/take-act";
import { applyMenu, splitWays } from "../src/state/store";

/** The menu read of a store with nothing on it, saying whether it takes a bill in parts. */
function menu(partPayments: boolean | undefined): MenuResponse {
  return {
    items: [],
    modifier_groups: [],
    courses: [],
    currency: "VND",
    part_payments: partPayments,
  } as unknown as MenuResponse;
}

const share = {
  method: "PAYMENT_METHOD_QR",
  tendered: { currency_code: "VND", amount_minor: 50_000 },
  applied_to_bill: { currency_code: "VND", amount_minor: 50_000 },
};

afterEach(() => applyMenu(menu(true)));

describe("a share of an even split", () => {
  it("is taken while the bill owes at least as much", () => {
    expect(takeRefusal(share, 100_000)).toBeNull();
    expect(takeRefusal(share, 50_000)).toBeNull();
  });

  it("is refused before it is sent when it is more than the bill still owes", () => {
    expect(takeRefusal(share, 49_999)).toMatch(/./);
  });
});

describe("the even split's guest counts", () => {
  it("are offered where the store takes a bill in parts", () => {
    applyMenu(menu(true));
    expect(splitWays()).toEqual([2, 3, 4, 5, 6]);
  });

  it("are not offered where it takes a bill in one payment, or by an edge that says nothing", () => {
    applyMenu(menu(false));
    expect(splitWays()).toEqual([]);
    applyMenu(menu(undefined));
    expect(splitWays()).toEqual([]);
  });
});
