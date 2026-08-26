// Small presentation helpers. Locale-aware integer formatting for the rollup counts; the app carries
// no money display today (the daily rollup is event counts, not revenue), so there is no currency
// formatter here yet — it arrives with the sales rollup that reports totals.

import { locale } from "../i18n";
import type { Money } from "../api/types";

/** A count, grouped for the active locale (e.g. `1,234`). */
export function formatCount(value: number): string {
  return new Intl.NumberFormat(locale()).format(value);
}

/**
 * An integer `Money` value, grouped for the active locale with its currency code appended (e.g.
 * `150,000 VND`). `amount_minor` is the currency's smallest unit; for VND (v1) that is a whole đồng,
 * so the grouped integer is the price the operator reads. Currencies with a fractional minor unit are
 * shown in minor units — the same units the price is authored in — since the exponent is a locale-pack
 * property this presentation layer does not carry.
 */
export function formatMoney(money: Money): string {
  return `${new Intl.NumberFormat(locale()).format(money.amount_minor)} ${money.currency_code}`;
}
