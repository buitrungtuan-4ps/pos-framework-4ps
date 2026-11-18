// `state/scan-reader.ts` and `state/codes.ts` (ADR-0222 decision 2): digits within the store's
// window ending in Enter are a scan and anything slower is typing; the menu's barcodes and PLUs
// resolve to their item without asking the edge.

import { describe, expect, it } from "vitest";

import type { MenuItemResponse } from "../src/api/schemas";
import { indexCodes, resolveCode, scanWindowMs, scansListened } from "../src/state/codes";
import { createScanReader } from "../src/state/scan-reader";

/** Feeds `keys` `gap` ms apart and answers what the last one completed. */
function feed(keys: string[], gap: number, window = 50): string | null {
  const reader = createScanReader(() => window);
  let answer: string | null = null;
  keys.forEach((key, index) => {
    answer = reader.key(key, 1_000 + index * gap);
  });
  return answer;
}

describe("a scan", () => {
  it("is digits and Enter, each within the window", () => {
    expect(feed([..."96385074", "Enter"], 10)).toBe("96385074");
    expect(feed([..."4011", "Enter"], 30, 50)).toBe("4011");
  });

  it("is not typing slower than the window, a letter, or Enter alone", () => {
    expect(feed([..."96385074", "Enter"], 120)).toBeNull();
    expect(feed([..."9638a074", "Enter"], 10)).toBeNull();
    expect(feed(["Enter"], 10)).toBeNull();
  });
});

describe("the code index", () => {
  const item = (id: string, codes: Partial<MenuItemResponse>) => ({ menu_item_id: id, ...codes }) as MenuItemResponse;

  it("resolves a barcode or a PLU to its item, and takes the store's window", () => {
    indexCodes([item("bread", { barcodes: ["96385074"], plu: "4011" }), item("cake", {})], 80, true);
    expect(resolveCode("96385074")).toBe("bread");
    expect(resolveCode("4011")).toBe("bread");
    expect(resolveCode("12345")).toBeNull();
    expect(scanWindowMs()).toBe(80);
    expect(scansListened()).toBe(true);
  });

  it("falls back to 50 ms and listens only where the store sells by barcode", () => {
    indexCodes([], 500, undefined);
    expect(scanWindowMs()).toBe(50);
    expect(scansListened()).toBe(false);
  });
});
