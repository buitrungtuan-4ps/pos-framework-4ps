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
 * A past instant as locale-aware relative text (e.g. `5 minutes ago`, `2 hours ago`). `seconds` is how
 * long ago the instant was (a non-negative age). Uses `Intl.RelativeTimeFormat`, so the phrasing is
 * localized by the platform without a catalogue entry per unit; the fleet view's "last seen" and the
 * health view's "last tick" both read through it.
 */
export function formatRelativeAge(seconds: number): string {
  const rtf = new Intl.RelativeTimeFormat(locale(), { numeric: "auto" });
  const age = Math.max(0, Math.round(seconds));
  if (age < 60) {
    return rtf.format(-age, "second");
  }
  if (age < 3600) {
    return rtf.format(-Math.round(age / 60), "minute");
  }
  if (age < 86_400) {
    return rtf.format(-Math.round(age / 3600), "hour");
  }
  return rtf.format(-Math.round(age / 86_400), "day");
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
