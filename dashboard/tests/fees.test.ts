// What the Fees screen sends is what will run ([ADR-0159](../../docs/adr/0159-a-fee-is-configuration.md)).
//
// The form is a draft and the wire is a rule; `lib/fees.ts` is the one place the two meet, and each
// way it could go wrong quietly is a pricing mistake, so each is pinned here:
//
//   * **the owner's defaults.** A new fee is taxed the way its lines are, after discounts, net of
//     tax, and not waivable (decided 2026-10-01). A form that started anywhere else would publish a
//     different fee than the one the owner agreed to by default;
//   * **a rate through a float.** `2.5` must become the exact ratio `250/10000`, never `0.025`: the
//     platform keeps every price out of floating point (AGENTS.md §2), and a fee is part of a price;
//   * **a field that does not apply.** A percentage's base on an amount, an item list under "every
//     line", a tax class under "taxed the way its lines are": sent anyway, each would read as a
//     setting the fee has when it has none;
//   * **the sample bill's menu.** Its lines are what the store's published book sells on the channel,
//     or the book's fallback, as the till prices them.

import { describe, expect, it } from "vitest";

import type { FeeRuleFields, Json } from "../src/api/types";
import {
  draftFromRule,
  draftGap,
  type FeeDraft,
  menuOffers,
  newDraft,
  percentText,
  ratioFromPercent,
  ruleFromDraft,
  storeCurrency,
} from "../src/lib/fees";

/** A draft complete enough to send, with `fields` over it. */
function ready(fields: Partial<FeeDraft> = {}): FeeDraft {
  return { ...newDraft("VND"), code: "SVC", displayName: "Service charge", percent: "5", ...fields };
}

describe("a new fee", () => {
  it("starts from the owner's defaults", () => {
    const draft = newDraft("VND");
    expect(draft.kind).toBe("FEE_KIND_PERCENT");
    expect(draft.tax).toBe("FEE_TAX_FOLLOW_LINES");
    expect(draft.baseDiscounted).toBe(true);
    expect(draft.baseTaxInclusive).toBe(false);
    expect(draft.waivable).toBe(false);
    expect(draft.active).toBe(true);
    expect(draft.itemScope).toBe("FEE_ITEMS_ALL");
    expect(draft.channels).toEqual([]);
    expect(draft.feeId).toBeNull();
    expect(draft.currency).toBe("VND");
  });

  it("is sent with those defaults spelled out, and without an id for the cloud to mint", () => {
    expect(ruleFromDraft(ready())).toEqual({
      code: "SVC",
      display_name: "Service charge",
      display_name_translations: {},
      kind: "FEE_KIND_PERCENT",
      rate: { numerator: 5, denominator: 100 },
      channels: [],
      item_scope: "FEE_ITEMS_ALL",
      menu_item_ids: [],
      item_category_ids: [],
      base_discounted: true,
      base_tax_inclusive: false,
      tax: "FEE_TAX_FOLLOW_LINES",
      waivable: false,
      active: true,
    });
  });
});

describe("a rate typed as a percentage", () => {
  it("is kept as an exact ratio, whole percentages over a hundred", () => {
    expect(ratioFromPercent("5")).toEqual({ numerator: 5, denominator: 100 });
    expect(ratioFromPercent("0")).toEqual({ numerator: 0, denominator: 100 });
    expect(ratioFromPercent("100")).toEqual({ numerator: 100, denominator: 100 });
    expect(ratioFromPercent(" 10.00 ")).toEqual({ numerator: 10, denominator: 100 });
  });

  it("and a fraction of one in basis points, never as a float", () => {
    expect(ratioFromPercent("2.5")).toEqual({ numerator: 250, denominator: 10_000 });
    expect(ratioFromPercent("2,5")).toEqual({ numerator: 250, denominator: 10_000 });
    expect(ratioFromPercent("0.01")).toEqual({ numerator: 1, denominator: 10_000 });
    expect(ratioFromPercent("12.75")).toEqual({ numerator: 1275, denominator: 10_000 });
    for (const typed of ["2.5", "0.01", "12.75", "99.99"]) {
      const rate = ratioFromPercent(typed);
      expect(Number.isInteger(rate?.numerator)).toBe(true);
      expect(Number.isInteger(rate?.denominator)).toBe(true);
    }
  });

  it("is refused when it is not a percentage from 0 to 100 with at most two decimals", () => {
    for (const typed of ["", " ", "abc", "-1", "100.01", "101", "1.234", "1e2", "5%", ".5", "5."]) {
      expect(ratioFromPercent(typed)).toBeNull();
    }
  });

  it("reads back as the percentage it was typed as", () => {
    expect(percentText({ numerator: 5, denominator: 100 })).toBe("5");
    expect(percentText({ numerator: 250, denominator: 10_000 })).toBe("2.5");
    expect(percentText({ numerator: 1275, denominator: 10_000 })).toBe("12.75");
    expect(percentText({ numerator: 1, denominator: 10_000 })).toBe("0.01");
    expect(percentText({ numerator: 10, denominator: 1000 })).toBe("1");
  });

  it("shows a ratio no form writes as the fraction it is, rather than rounding it into another rate", () => {
    expect(percentText({ numerator: 1, denominator: 3 })).toBe("1/3");
  });
});

describe("what is sent", () => {
  it("drops a percentage's base from an amount, and carries the amount in its currency", () => {
    const rule = ruleFromDraft(
      ready({
        kind: "FEE_KIND_AMOUNT_PER_BILL",
        amountMinor: 15_000,
        currency: "vnd",
        baseDiscounted: false,
        baseTaxInclusive: true,
      }),
    );
    expect(rule.amount).toEqual({ currency_code: "VND", amount_minor: 15_000 });
    expect(rule.rate).toBeUndefined();
    expect(rule.base_discounted).toBe(true);
    expect(rule.base_tax_inclusive).toBe(false);
  });

  it("keeps a percentage's base as chosen", () => {
    const rule = ruleFromDraft(ready({ baseDiscounted: false, baseTaxInclusive: true }));
    expect(rule.base_discounted).toBe(false);
    expect(rule.base_tax_inclusive).toBe(true);
    expect(rule.amount).toBeUndefined();
  });

  it("sends an item list only where one is counted", () => {
    const listed = { menuItemIds: ["01ITEM"], categoryIds: ["01CATEGORY"] } as const;
    expect(ruleFromDraft(ready({ ...listed, itemScope: "FEE_ITEMS_ALL" }))).toMatchObject({
      menu_item_ids: [],
      item_category_ids: [],
    });
    expect(ruleFromDraft(ready({ ...listed, itemScope: "FEE_ITEMS_EXCLUDE" }))).toMatchObject({
      item_scope: "FEE_ITEMS_EXCLUDE",
      menu_item_ids: ["01ITEM"],
      item_category_ids: ["01CATEGORY"],
    });
  });

  it("names a tax class only for a fee taxed at one", () => {
    expect(ruleFromDraft(ready({ taxClassId: "01CLASS" })).tax_class_id).toBeUndefined();
    expect(
      ruleFromDraft(ready({ tax: "FEE_TAX_TAX_CLASS", taxClassId: "01CLASS" })).tax_class_id,
    ).toBe("01CLASS");
  });

  it("trims what was typed and drops a blank translation", () => {
    const rule = ruleFromDraft(
      ready({ code: " SVC ", displayName: " Phí phục vụ ", translations: { en: " Service ", ja: " " } }),
    );
    expect(rule.code).toBe("SVC");
    expect(rule.display_name).toBe("Phí phục vụ");
    expect(rule.display_name_translations).toEqual({ en: "Service" });
  });
});

describe("a written rule, edited", () => {
  const written: FeeRuleFields = {
    fee_id: "01FEE",
    code: "PKG",
    display_name: "Hộp",
    display_name_translations: { en: "Box" },
    kind: "FEE_KIND_AMOUNT_PER_UNIT",
    amount: { currency_code: "VND", amount_minor: 3000 },
    channels: ["SALES_CHANNEL_TAKEAWAY", "SALES_CHANNEL_DELIVERY"],
    item_scope: "FEE_ITEMS_INCLUDE",
    menu_item_ids: ["01ITEM"],
    item_category_ids: ["01CATEGORY"],
    base_discounted: true,
    base_tax_inclusive: false,
    tax: "FEE_TAX_TAX_CLASS",
    tax_class_id: "01CLASS",
    waivable: true,
    active: false,
  };

  it("is sent back as it was written, under its own id", () => {
    expect(ruleFromDraft(draftFromRule(written, "JPY"))).toEqual(written);
  });

  it("reads a field it leaves out as the default the wire reads it as", () => {
    const draft = draftFromRule({ fee_id: "01FEE", code: "SVC", display_name: "Service" }, "VND");
    expect(draft.tax).toBe("FEE_TAX_FOLLOW_LINES");
    expect(draft.baseDiscounted).toBe(true);
    expect(draft.baseTaxInclusive).toBe(false);
    expect(draft.waivable).toBe(false);
    expect(draft.active).toBe(true);
    expect(draft.currency).toBe("VND");
  });

  it("shows a rate as the percentage it is", () => {
    const draft = draftFromRule(
      { code: "SVC", display_name: "Service", rate: { numerator: 250, denominator: 10_000 } },
      "VND",
    );
    expect(draft.percent).toBe("2.5");
  });
});

describe("a draft that cannot be sent yet", () => {
  it("says the first thing it lacks, in the order the form reads", () => {
    expect(draftGap(ready({ code: " " }))).toBe("code");
    expect(draftGap(ready({ displayName: "" }))).toBe("displayName");
    expect(draftGap(ready({ percent: "150" }))).toBe("rate");
    expect(draftGap(ready({ kind: "FEE_KIND_AMOUNT_PER_BILL", currency: "", amountMinor: 1 }))).toBe(
      "currency",
    );
    expect(draftGap(ready({ kind: "FEE_KIND_AMOUNT_PER_BILL", amountMinor: null }))).toBe("amount");
    expect(draftGap(ready({ itemScope: "FEE_ITEMS_INCLUDE" }))).toBe("items");
    expect(draftGap(ready({ tax: "FEE_TAX_TAX_CLASS" }))).toBe("taxClass");
  });

  it("and nothing once it is complete", () => {
    expect(draftGap(ready())).toBeNull();
    expect(draftGap(ready({ kind: "FEE_KIND_AMOUNT_PER_UNIT", amountMinor: 0 }))).toBeNull();
  });
});

describe("the sample bill's menu", () => {
  const margherita = {
    menu_item_id: "01MARGHERITA",
    display_name: "Margherita",
    unit_price: { currency_code: "VND", amount_minor: 120_000 },
  };
  const tea = {
    menu_item_id: "01TEA",
    display_name: "Trà đá",
    unit_price: { currency_code: "VND", amount_minor: 10_000 },
  };
  const config: Json = {
    locale: { currency_code: "VND" },
    menu: {
      channels: [{ sales_channel: "SALES_CHANNEL_DINE_IN", catalog: { items: [margherita] } }],
      fallback: { items: [tea] },
    },
  };

  it("is what the channel's own catalog sells", () => {
    expect(menuOffers(config, "SALES_CHANNEL_DINE_IN")).toEqual([margherita]);
  });

  it("falls back to the book's fallback, as the till prices a channel with no row", () => {
    expect(menuOffers(config, "SALES_CHANNEL_DELIVERY")).toEqual([tea]);
  });

  it("is nothing for a store with no menu, rather than a guess", () => {
    expect(menuOffers(null, "SALES_CHANNEL_DINE_IN")).toEqual([]);
    expect(menuOffers({ locale: { currency_code: "VND" } }, "SALES_CHANNEL_DINE_IN")).toEqual([]);
  });

  it("is billed in the store's currency, once it has one", () => {
    expect(storeCurrency(config)).toBe("VND");
    expect(storeCurrency(null)).toBeNull();
    expect(storeCurrency({ locale: { currency_code: "dong" } })).toBeNull();
  });
});
