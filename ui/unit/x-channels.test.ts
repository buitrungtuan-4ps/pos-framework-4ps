import { describe, expect, it } from "vitest";

import type { XFigures } from "../src/api/schemas";
import { channelName, groupChannels, orderTypeName, tenderRows } from "../src/lib/x-channels";

const vnd = (amount_minor: number) => ({ currency_code: "VND", amount_minor });
const GRAB = "01J0000000000000000000GRAB";
const CAPICHI = "01J0000000000000000000CAP1";

const figures = {
  sales_amount: vnd(748_000),
  bill_count: 5,
  tenders: {
    cash: vnd(440_000),
    card: vnd(0),
    qr: vnd(0),
    other: vnd(308_000),
    other_than_apps: vnd(0),
    in_app: [
      { vendor_id: CAPICHI, amount: vnd(120_000) },
      { vendor_id: GRAB, name: "GrabFood", amount: vnd(188_000) },
    ],
  },
  channels: [
    {
      source: "SALE_SOURCE_APP",
      vendor_id: CAPICHI,
      order_type: "ORDER_TYPE_DELIVERY",
      bill_count: 1,
      sales_amount: vnd(120_000),
    },
    {
      source: "SALE_SOURCE_APP",
      vendor_id: GRAB,
      name: "GrabFood",
      order_type: "ORDER_TYPE_DELIVERY",
      bill_count: 1,
      sales_amount: vnd(188_000),
    },
    { source: "SALE_SOURCE_POS", order_type: "ORDER_TYPE_DINE_IN", bill_count: 1, sales_amount: vnd(200_000) },
    { source: "SALE_SOURCE_POS", order_type: "ORDER_TYPE_TAKEAWAY", bill_count: 1, sales_amount: vnd(90_000) },
    { source: "SALE_SOURCE_QR", order_type: "ORDER_TYPE_DINE_IN", bill_count: 1, sales_amount: vnd(150_000) },
  ],
} as unknown as XFigures;

// ADR-0259 decisions 4 and 5: the till's X names apps, not "other", and groups the edge's lines.
describe("the till's X by channel and order type", () => {
  it("names each app's in-app payments and leaves other without them", () => {
    expect(tenderRows(figures)).toEqual([
      ["Cash", vnd(440_000)],
      ["Card", vnd(0)],
      ["QR", vnd(0)],
      ["Paid via Delivery app …00CAP1", vnd(120_000)],
      ["Paid via GrabFood", vnd(188_000)],
      ["Other tenders", vnd(0)],
    ]);
  });

  it("groups the lines by channel and by type, to the same totals", () => {
    expect(figures.channels.map(channelName)).toEqual([
      "Delivery app …00CAP1",
      "GrabFood",
      "Store POS",
      "Store POS",
      "QR order",
    ]);
    expect(groupChannels(figures.channels, (line) => orderTypeName(line.order_type))).toEqual([
      ["Delivery", 2, vnd(308_000)],
      ["Dine-in", 2, vnd(350_000)],
      ["Take-away", 1, vnd(90_000)],
    ]);
    expect(orderTypeName("ORDER_TYPE_FROM_THE_FUTURE")).toBe("Type not recorded");
  });
});
