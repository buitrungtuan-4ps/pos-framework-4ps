// Switching QR ordering on warns when the published menu prices nothing for QR guests (O10).

import { describe, expect, it } from "vitest";

import { qrPriced } from "../src/screens/Channels";

const item = { menu_item_id: "01KITEM", unit_price: { currency_code: "VND", amount_minor: 99_000 } };

describe("whether the menu prices anything for QR guests", () => {
  it("reads a QR row with an item as priced, and an empty one as not", () => {
    expect(qrPriced({ menu: { channels: [{ sales_channel: "SALES_CHANNEL_QR", catalog: { items: [item] } }] } })).toBe(true);
    expect(
      qrPriced({
        menu: { channels: [{ sales_channel: "SALES_CHANNEL_QR", catalog: { items: [] } }], fallback: { items: [item] } },
      }),
    ).toBe(false);
  });

  it("falls back to the fallback catalog when there is no QR row", () => {
    expect(qrPriced({ menu: { channels: [{ sales_channel: "SALES_CHANNEL_DINE_IN", catalog: { items: [item] } }], fallback: { items: [item] } } })).toBe(true);
    expect(qrPriced({ menu: { channels: [{ sales_channel: "SALES_CHANNEL_DINE_IN", catalog: { items: [item] } }] } })).toBe(false);
  });

  it("reads no menu at all as not priced", () => {
    expect(qrPriced(null)).toBe(false);
    expect(qrPriced({})).toBe(false);
  });
});
