// Fees ([ADR-0159](../../../docs/adr/0159-a-fee-is-configuration.md)): what the Fees screen needs
// that is not drawing.
//
// - **The form a rule is edited in.** A draft holds what the operator typed; `ruleFromDraft` turns
//   it into the body `/admin/fees` takes and `draftFromRule` turns a written rule back into a draft.
//   A new rule starts from the owner's defaults (2026-10-01): taxed the way its lines are, after
//   discounts, net of tax, not waivable, and in force.
// - **A rate typed as a percentage, kept as an exact ratio.** `2.5` becomes `{ 250, 10000 }`, never
//   the float `0.025`: a rate is part of a price, and the platform keeps prices out of floating
//   point (AGENTS.md §2). Whole percentages are kept as `{ n, 100 }`, as the cloud writes them.
// - **What a store sells on a channel**, read from its published `menu` node, which a sample bill
//   is made from — the same book the store prices a line from.
//
// Nothing here is personal data: a fee's code, its names, its figures and the items it counts.

import type {
  FeeItems,
  FeeKind,
  FeeRuleFields,
  FeeTax,
  Json,
  Money,
  Ratio,
  SalesChannel,
} from "../api/types";

/** A rule as the form edits it. */
export interface FeeDraft {
  /** The fee being edited or overridden; `null` for a fee not created yet. */
  readonly feeId: string | null;
  readonly code: string;
  readonly displayName: string;
  /** The name per locale, as typed; a blank one is dropped when the rule is saved. */
  readonly translations: Readonly<Record<string, string>>;
  readonly kind: FeeKind;
  /** For a percentage: the rate as the operator typed it, `5` or `2.5`. */
  readonly percent: string;
  /** For an amount: the amount in minor units, or `null` while none is typed. */
  readonly amountMinor: number | null;
  /** The amount's currency. */
  readonly currency: string;
  /** The channels it applies on; empty is every channel. */
  readonly channels: readonly SalesChannel[];
  readonly itemScope: FeeItems;
  readonly menuItemIds: readonly string[];
  readonly categoryIds: readonly string[];
  /** For a percentage: taken after discounts and comps (`true`) or before them. */
  readonly baseDiscounted: boolean;
  /** For a percentage: taken on the tax-inclusive price (`true`) or net of tax. */
  readonly baseTaxInclusive: boolean;
  readonly tax: FeeTax;
  /** For `FEE_TAX_TAX_CLASS`: the class, or `""` while none is chosen. */
  readonly taxClassId: string;
  readonly waivable: boolean;
  readonly active: boolean;
}

/** The kinds a rule can be, in the order the form offers them. */
export const FEE_KINDS: readonly FeeKind[] = [
  "FEE_KIND_PERCENT",
  "FEE_KIND_AMOUNT_PER_BILL",
  "FEE_KIND_AMOUNT_PER_UNIT",
];

/** The item scopes, in the order the form offers them. */
export const FEE_ITEM_SCOPES: readonly FeeItems[] = [
  "FEE_ITEMS_ALL",
  "FEE_ITEMS_INCLUDE",
  "FEE_ITEMS_EXCLUDE",
];

/** The tax treatments, in the order the form offers them. */
export const FEE_TAXES: readonly FeeTax[] = [
  "FEE_TAX_FOLLOW_LINES",
  "FEE_TAX_NOT_TAXABLE",
  "FEE_TAX_TAX_CLASS",
];

/**
 * A new rule from the owner's defaults: a percentage of every line on every channel, taken after
 * discounts and net of tax, taxed the way its lines are, not waivable, and in force.
 */
export function newDraft(currency: string): FeeDraft {
  return {
    feeId: null,
    code: "",
    displayName: "",
    translations: {},
    kind: "FEE_KIND_PERCENT",
    percent: "",
    amountMinor: null,
    currency,
    channels: [],
    itemScope: "FEE_ITEMS_ALL",
    menuItemIds: [],
    categoryIds: [],
    baseDiscounted: true,
    baseTaxInclusive: false,
    tax: "FEE_TAX_FOLLOW_LINES",
    taxClassId: "",
    waivable: false,
    active: true,
  };
}

/** One of `allowed`, or `fallback` for a token this console does not know. */
function known<T extends string>(token: string | undefined, allowed: readonly T[], fallback: T): T {
  return allowed.find((value) => value === token) ?? fallback;
}

/**
 * A written rule as a draft, to edit it — or, with `feeId` kept and another scope chosen, to
 * override it there. Absent fields read as the defaults the wire reads them as.
 */
export function draftFromRule(rule: FeeRuleFields, fallbackCurrency: string): FeeDraft {
  const kind = known(rule.kind, FEE_KINDS, "FEE_KIND_PERCENT");
  return {
    feeId: rule.fee_id ?? null,
    code: rule.code,
    displayName: rule.display_name,
    translations: { ...(rule.display_name_translations ?? {}) },
    kind,
    percent: rule.rate === undefined ? "" : percentText(rule.rate),
    amountMinor: rule.amount?.amount_minor ?? null,
    currency: rule.amount?.currency_code ?? fallbackCurrency,
    channels: (rule.channels ?? []).filter((channel): channel is SalesChannel =>
      channel.startsWith("SALES_CHANNEL_"),
    ),
    itemScope: known(rule.item_scope, FEE_ITEM_SCOPES, "FEE_ITEMS_ALL"),
    menuItemIds: [...(rule.menu_item_ids ?? [])],
    categoryIds: [...(rule.item_category_ids ?? [])],
    baseDiscounted: rule.base_discounted ?? true,
    baseTaxInclusive: rule.base_tax_inclusive ?? false,
    tax: known(rule.tax, FEE_TAXES, "FEE_TAX_FOLLOW_LINES"),
    taxClassId: rule.tax_class_id ?? "",
    waivable: rule.waivable ?? false,
    active: rule.active ?? true,
  };
}

/** Why a draft cannot be sent yet, in the order the form reads; `null` when it can. */
export type DraftGap =
  | "code"
  | "displayName"
  | "rate"
  | "currency"
  | "amount"
  | "items"
  | "taxClass";

/** The first thing a draft lacks, before the cloud would refuse it for it. */
export function draftGap(draft: FeeDraft): DraftGap | null {
  if (draft.code.trim() === "") {
    return "code";
  }
  if (draft.displayName.trim() === "") {
    return "displayName";
  }
  if (draft.kind === "FEE_KIND_PERCENT") {
    if (ratioFromPercent(draft.percent) === null) {
      return "rate";
    }
  } else if (!/^[A-Z]{3}$/.test(draft.currency.trim().toUpperCase())) {
    return "currency";
  } else if (draft.amountMinor === null || draft.amountMinor < 0) {
    return "amount";
  }
  if (
    draft.itemScope !== "FEE_ITEMS_ALL" &&
    draft.menuItemIds.length === 0 &&
    draft.categoryIds.length === 0
  ) {
    return "items";
  }
  if (draft.tax === "FEE_TAX_TAX_CLASS" && draft.taxClassId === "") {
    return "taxClass";
  }
  return null;
}

/**
 * The body `/admin/fees` takes for a draft. A field the kind, item scope or tax treatment ignores
 * is left out, as the cloud would drop it, so what is sent is what will run.
 */
export function ruleFromDraft(draft: FeeDraft): FeeRuleFields {
  const percent = draft.kind === "FEE_KIND_PERCENT";
  const listed = draft.itemScope !== "FEE_ITEMS_ALL";
  const translations = Object.fromEntries(
    Object.entries(draft.translations)
      .map(([locale, name]) => [locale.trim(), name.trim()] as const)
      .filter(([locale, name]) => locale !== "" && name !== ""),
  );
  const rate = percent ? ratioFromPercent(draft.percent) : null;
  const amount: Money | null =
    !percent && draft.amountMinor !== null
      ? { currency_code: draft.currency.trim().toUpperCase(), amount_minor: draft.amountMinor }
      : null;
  return {
    ...(draft.feeId === null ? {} : { fee_id: draft.feeId }),
    code: draft.code.trim(),
    display_name: draft.displayName.trim(),
    display_name_translations: translations,
    kind: draft.kind,
    ...(rate === null ? {} : { rate }),
    ...(amount === null ? {} : { amount }),
    channels: [...draft.channels],
    item_scope: draft.itemScope,
    menu_item_ids: listed ? [...draft.menuItemIds] : [],
    item_category_ids: listed ? [...draft.categoryIds] : [],
    base_discounted: percent ? draft.baseDiscounted : true,
    base_tax_inclusive: percent ? draft.baseTaxInclusive : false,
    tax: draft.tax,
    ...(draft.tax === "FEE_TAX_TAX_CLASS" && draft.taxClassId !== ""
      ? { tax_class_id: draft.taxClassId }
      : {}),
    waivable: draft.waivable,
    active: draft.active,
  };
}

/** The most decimal places a typed percentage keeps: basis points, which is what a rate is quoted in. */
const PERCENT_DECIMALS = 2;

/**
 * A percentage as typed, `5` or `2.5` (or `2,5`), as an exact ratio: `{ 5, 100 }` for a whole
 * percentage, `{ 250, 10000 }` otherwise. `null` for anything that is not a number from 0 to 100
 * with at most two decimals — read as text, digit by digit, so no float ever holds a rate.
 */
export function ratioFromPercent(typed: string): Ratio | null {
  const text = typed.trim().replace(",", ".");
  const match = /^(\d{1,3})(?:\.(\d{1,2}))?$/.exec(text);
  if (match === null) {
    return null;
  }
  const whole = Number.parseInt(match[1] ?? "0", 10);
  const fraction = (match[2] ?? "").padEnd(PERCENT_DECIMALS, "0");
  const basisPoints = whole * 100 + Number.parseInt(fraction, 10);
  if (basisPoints > 10_000) {
    return null;
  }
  return basisPoints % 100 === 0
    ? { numerator: basisPoints / 100, denominator: 100 }
    : { numerator: basisPoints, denominator: 10_000 };
}

/**
 * An exact ratio as the percentage it is, for the form and the list: `{ 5, 100 }` is `5`,
 * `{ 250, 10000 }` is `2.5`. A ratio that is not a whole number of basis points, which no form
 * writes, is shown as the fraction it is rather than rounded into a different rate.
 */
export function percentText(rate: Ratio): string {
  const { numerator, denominator } = rate;
  if (denominator <= 0 || !Number.isInteger(numerator) || !Number.isInteger(denominator)) {
    return `${numerator}/${denominator}`;
  }
  const scaled = numerator * 10_000;
  if (scaled % denominator !== 0) {
    return `${numerator}/${denominator}`;
  }
  const basisPoints = scaled / denominator;
  const whole = Math.trunc(basisPoints / 100);
  const fraction = Math.abs(basisPoints % 100);
  if (fraction === 0) {
    return String(whole);
  }
  return `${whole}.${String(fraction).padStart(2, "0").replace(/0$/, "")}`;
}

/** One item a store's menu sells on a channel. */
export interface MenuOffer {
  readonly menu_item_id: string;
  readonly display_name: string;
  readonly unit_price: Money | null;
}

/** An object's own field, when the value is an object. */
function field(value: Json | undefined, key: string): Json | undefined {
  return value !== null && typeof value === "object" && !Array.isArray(value)
    ? (value as Record<string, Json>)[key]
    : undefined;
}

/** A catalog's entries, read loosely: an entry without an id is skipped rather than guessed at. */
function offersIn(catalog: Json | undefined): MenuOffer[] {
  const items = field(catalog, "items");
  if (!Array.isArray(items)) {
    return [];
  }
  return items.flatMap((entry) => {
    const id = field(entry, "menu_item_id");
    const name = field(entry, "display_name");
    const price = field(entry, "unit_price");
    const currency = field(price, "currency_code");
    const minor = field(price, "amount_minor");
    if (typeof id !== "string") {
      return [];
    }
    return [
      {
        menu_item_id: id,
        display_name: typeof name === "string" ? name : id,
        unit_price:
          typeof currency === "string" && typeof minor === "number"
            ? { currency_code: currency, amount_minor: minor }
            : null,
      },
    ];
  });
}

/**
 * What a store's published `menu` node sells on `channel`: the channel's own catalog, or the
 * book's fallback when the channel has none — the catalog the store prices a line from
 * (`MenuBook::catalog_for`). Empty for a store with no menu.
 */
export function menuOffers(config: Json | null | undefined, channel: SalesChannel): MenuOffer[] {
  const menu = field(config ?? undefined, "menu");
  const rows = field(menu, "channels");
  if (Array.isArray(rows)) {
    const row = rows.find((candidate) => field(candidate, "sales_channel") === channel);
    if (row !== undefined) {
      return offersIn(field(row, "catalog"));
    }
  }
  return offersIn(field(menu, "fallback"));
}

/** The currency a store bills in, from its published `locale` node, or `null` before it has one. */
export function storeCurrency(config: Json | null | undefined): string | null {
  const code = field(field(config ?? undefined, "locale"), "currency_code");
  return typeof code === "string" && /^[A-Z]{3}$/.test(code) ? code : null;
}
