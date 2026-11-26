import { describe, expect, it } from "vitest";

import { tenderSplit } from "../src/screens/reports/XzCash";
import type { XzReport } from "../src/api/schemas";

// ADR-0248 decision 2: what delivery apps took is read apart from the store's own tenders.
describe("the X/Z tender split", () => {
  it("reads paid via delivery apps apart from other tenders", () => {
    const report = {
      revenue: {
        by_tender: {
          PAYMENT_METHOD_CASH: 100_000,
          PAYMENT_METHOD_VENDOR: 132_000,
          PAYMENT_METHOD_VOUCHER: 50_000,
        },
      },
    } as unknown as XzReport;
    expect(tenderSplit(report)).toEqual([
      ["reports.tenderCash", 100_000],
      ["reports.tenderCard", 0],
      ["reports.tenderQr", 0],
      ["reports.tenderVendor", 132_000],
      ["reports.tenderOther", 50_000],
    ]);
  });
});
