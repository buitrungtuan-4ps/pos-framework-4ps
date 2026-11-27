import { describe, expect, it } from "vitest";

import { setLocale } from "../src/i18n";
import { channelName, orderTypeName } from "../src/lib/x-channels";
import {
  applyOrderChannels,
  channelOf,
  listsBeyondBuiltIns,
  offeredTypes,
  typeLabel,
  type OfferedType,
} from "../src/state/order-channels";

// Fake configuration only (ADR-0260).
const pickup: OfferedType = {
  order_type: "ORDER_TYPE_PICKUP",
  base_order_type: "ORDER_TYPE_TAKEAWAY",
  names: { en: "Pick-up", vi: "Tự đến lấy" },
  default: true,
};
const dineIn: OfferedType = {
  order_type: "ORDER_TYPE_DINE_IN",
  base_order_type: "ORDER_TYPE_DINE_IN",
  names: {},
  default: false,
};

describe("the store's order types and sources at the till", () => {
  it("asks only where the store lists more than the built-ins", () => {
    applyOrderChannels({});
    expect(listsBeyondBuiltIns()).toBe(false);
    applyOrderChannels({ order_types: [dineIn], sale_sources: [] });
    expect(listsBeyondBuiltIns()).toBe(false);
    applyOrderChannels({ order_types: [pickup, dineIn], sale_sources: [] });
    expect(listsBeyondBuiltIns()).toBe(true);
    expect(offeredTypes().map((offered) => offered.order_type)).toEqual(["ORDER_TYPE_PICKUP", "ORDER_TYPE_DINE_IN"]);
    applyOrderChannels({ order_types: [dineIn], sale_sources: [{ source: "SALE_SOURCE_HOTLINE", names: { en: "Hotline" } }] });
    expect(listsBeyondBuiltIns()).toBe(true);
  });

  it("opens a type on its base kind's channel and names it in the till's language", () => {
    expect(channelOf(pickup)).toBe("SALES_CHANNEL_TAKEAWAY");
    expect(channelOf(dineIn)).toBe("SALES_CHANNEL_DINE_IN");
    setLocale("vi");
    expect(typeLabel(pickup)).toBe("Tự đến lấy");
    expect(typeLabel(dineIn)).toBe("Ăn tại chỗ");
    setLocale("en");
    expect(typeLabel(pickup)).toBe("Pick-up");
  });

  it("names an X line by the store's names, else the till's words", () => {
    expect(orderTypeName("ORDER_TYPE_PICKUP", { en: "Pick-up" })).toBe("Pick-up");
    expect(orderTypeName("ORDER_TYPE_DINE_IN")).toBe("Dine-in");
    const line = {
      source: "SALE_SOURCE_HOTLINE",
      source_names: { en: "Hotline", vi: "Tổng đài" },
      order_type: "ORDER_TYPE_PICKUP",
      bill_count: 1,
      sales_amount: { currency_code: "VND", amount_minor: 70_000 },
    };
    expect(channelName(line)).toBe("Hotline");
    expect(channelName({ ...line, source: "SALE_SOURCE_KIOSK", source_names: undefined })).toBe("Kiosk");
  });
});
