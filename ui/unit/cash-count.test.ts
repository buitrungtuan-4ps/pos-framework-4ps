// `state/cash-count.ts` (ADR-0186 decisions 1 and 3): a count field records the typed figure where the
// store has no counting list, and the counter's own total and rows where it has one.

import { beforeEach, describe, expect, it } from "vitest";

import { applyCountDenominations, countDenominations, EMPTY_COUNT, fieldAmount } from "../src/state/cash-count";
import { money } from "../src/lib/money";

beforeEach(() => {
  applyCountDenominations(undefined);
});

describe("applyCountDenominations", () => {
  it("reads an absent list, from an older edge, as no counter", () => {
    applyCountDenominations([10_000, 50_000]);
    applyCountDenominations(undefined);
    expect(countDenominations()).toEqual([]);
  });
});

describe("fieldAmount without a counting list", () => {
  it("records the typed figure in minor units, and nothing else", () => {
    // The demo store's currency is VND, whose minor unit is the đồng itself.
    expect(fieldAmount("1,250,000", EMPTY_COUNT)).toEqual({ minor: 1_250_000 });
  });

  it("is null while the text is not a figure", () => {
    expect(fieldAmount("", EMPTY_COUNT)).toBeNull();
    expect(fieldAmount("12a", EMPTY_COUNT)).toBeNull();
  });
});

describe("fieldAmount with a counting list", () => {
  const rows = [
    { denomination: money("VND", 500_000), quantity: 2 },
    { denomination: money("VND", 10_000), quantity: 3 },
  ];

  it("records the counter's total and rows and ignores the typed text", () => {
    applyCountDenominations([10_000, 500_000]);
    const recorded = fieldAmount("999", { total: 1_030_000, rows, uncounted: 0 });
    expect(recorded).toEqual({
      minor: 1_030_000,
      breakdown: { denomination_counts: rows, uncounted_amount: undefined },
    });
  });

  it("names the amount the list does not cover in the store's currency", () => {
    applyCountDenominations([10_000, 500_000]);
    const recorded = fieldAmount("", { total: 1_030_500, rows, uncounted: 500 });
    expect(recorded?.breakdown?.uncounted_amount).toEqual(money("VND", 500));
  });

  it("hands over a copy of the rows, not the counter's own array", () => {
    applyCountDenominations([10_000, 500_000]);
    const recorded = fieldAmount("", { total: 1_030_000, rows, uncounted: 0 });
    expect(recorded?.breakdown?.denomination_counts).not.toBe(rows);
  });
});
