// Small presentation helpers. Locale-aware integer formatting for the rollup counts; the app carries
// no money display today (the daily rollup is event counts, not revenue), so there is no currency
// formatter here yet — it arrives with the sales rollup that reports totals.

import { locale } from "../i18n";

/** A count, grouped for the active locale (e.g. `1,234`). */
export function formatCount(value: number): string {
  return new Intl.NumberFormat(locale()).format(value);
}
