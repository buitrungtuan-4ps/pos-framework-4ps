// The till's Vietnamese catalogue, read in Node with no browser and no edge (UX-8).
//
// A Vietnamese till said "Order này đã có hoá đơn đang mở": an English noun inside a Vietnamese
// sentence, where every other screen says "đơn" (the counter's "Đơn tại quầy", "Đơn tiếp theo").
// This holds the catalogue to the one word.

import { readFileSync } from "node:fs";

import { expect, test } from "@playwright/test";

const vi = JSON.parse(readFileSync(new URL("../src/i18n/vi.json", import.meta.url), "utf8"));

test("the Vietnamese catalogue says đơn for an order, never the English word", () => {
  const english = Object.entries(vi)
    .filter(([, text]) => /\border\b/i.test(text))
    .map(([key]) => key);
  expect(english).toEqual([]);
});
