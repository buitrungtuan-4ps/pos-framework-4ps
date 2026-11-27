import { describe, expect, it } from "vitest";

import type { XzReport } from "../src/api/schemas";
import { tenderSplit } from "../src/screens/reports/XzCash";
import { channelName, groupLines, orderTypeName } from "../src/screens/reports/XzChannels";

const GRAB = "01J0000000000000000000GRAB";
const CAPICHI = "01J0000000000000000000CAP1";

const report = {
  vendor_names: { [GRAB]: "GrabFood" },
  revenue: {
    by_tender: {
      PAYMENT_METHOD_CASH: 290_000,
      PAYMENT_METHOD_VENDOR: 308_000,
    },
    by_app_tender: { [GRAB]: 188_000, [CAPICHI]: 120_000 },
    by_channel: [
      {
        source: "SALE_SOURCE_APP",
        vendor_id: CAPICHI,
        order_type: "ORDER_TYPE_DELIVERY",
        bills: 1,
        sales: 120_000,
      },
      {
        source: "SALE_SOURCE_APP",
        vendor_id: GRAB,
        order_type: "ORDER_TYPE_DELIVERY",
        bills: 1,
        sales: 188_000,
      },
      {
        source: "SALE_SOURCE_POS",
        order_type: "ORDER_TYPE_DINE_IN",
        bills: 1,
        sales: 200_000,
      },
      {
        source: "SALE_SOURCE_POS",
        order_type: "ORDER_TYPE_TAKEAWAY",
        bills: 1,
        sales: 90_000,
      },
      {
        source: "SALE_SOURCE_QR",
        order_type: "ORDER_TYPE_DINE_IN",
        bills: 1,
        sales: 150_000,
      },
    ],
  },
} as unknown as XzReport;

// ADR-0259 decisions 4 and 5.
describe("the X/Z by channel and order type", () => {
  it("names an app by its connection, and one the store no longer lists by its id", () => {
    const lines = report.revenue.by_channel ?? [];
    expect(lines.map((line) => channelName(report, line))).toEqual([
      "Delivery app …00CAP1",
      "GrabFood",
      "Store POS",
      "Store POS",
      "QR order",
    ]);
    expect(orderTypeName("ORDER_TYPE_TAKEAWAY")).toBe("Take-away");
    expect(orderTypeName("ORDER_TYPE_FROM_THE_FUTURE")).toBe("Type not recorded");
  });

  it("groups the lines by channel and by type, to the same totals", () => {
    const lines = report.revenue.by_channel ?? [];
    expect(groupLines(lines, (line) => channelName(report, line))).toEqual([
      ["Delivery app …00CAP1", 1, 120_000],
      ["GrabFood", 1, 188_000],
      ["Store POS", 2, 290_000],
      ["QR order", 1, 150_000],
    ]);
    expect(groupLines(lines, (line) => orderTypeName(line.order_type))).toEqual([
      ["Delivery", 2, 308_000],
      ["Dine-in", 2, 350_000],
      ["Take-away", 1, 90_000],
    ]);
  });

  it("splits in-app payments by app instead of one delivery-apps row", () => {
    expect(tenderSplit(report, (id) => (id === GRAB ? "GrabFood" : id))).toEqual([
      ["reports.tenderCash", 290_000],
      ["reports.tenderCard", 0],
      ["reports.tenderQr", 0],
      ["reports.tenderPaidVia", 120_000, { app: CAPICHI }],
      ["reports.tenderPaidVia", 188_000, { app: "GrabFood" }],
      ["reports.tenderOther", 0],
    ]);
  });
});
