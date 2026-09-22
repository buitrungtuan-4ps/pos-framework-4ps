// What the console needs to read and write money as money: how many decimal places each currency
// has ([ADR-0134](../../../docs/adr/0134-a-currency-says-how-many-decimals-it-has.md),
// [ADR-0135](../../../docs/adr/0135-the-console-reads-money-the-way-the-till-does.md)).
//
// The till holds a single exponent, because a till serves one store in one currency. The console
// holds a **map**: it spans every country the platform ships a pack for, and a figure on the reports
// screen belongs to whichever store it came from. That is the one deliberate difference between the
// two surfaces, and ADR-0135 records it.
//
// The values arrive from `GET /admin/countries`, the same read the currency picker and the store
// settings form already make; `Shell` feeds them in once the authenticated frame mounts. This module
// holds no HTTP client of its own on purpose — `components/ui.tsx` reads it, and `ui.tsx` is in the
// shell chunk, where the `Badge` comment records what pulling a fat dependency in costs.

import { createSignal } from "solid-js";

import type { Country, Money } from "../api/types";
import { formatMoney, parseMoney } from "../lib/format";

// How many decimal places a currency has, for the window before `GET /admin/countries` answers.
//
// The same table the till carries, and it is a fallback there for the same reason: never blank, and
// never the authority. The `?? 0` that used to *be* the authority on both surfaces was right for the
// đồng and the yen and silently wrong for the rupee — ₹261.45 drawn as `26,145` — which is the whole
// of what ADR-0134 and ADR-0135 exist to close.
//
// A currency in neither the fetched map nor this table gets `0`: the arithmetic-neutral answer
// rather than a claim. That window is the app's first read, and a store always belongs to a country
// the platform ships a pack for.
const FALLBACK_EXPONENT: Record<string, number> = { VND: 0, JPY: 0, INR: 2, USD: 2 };

const [exponents, setExponents] = createSignal<Record<string, number>>({});

/**
 * How many decimal places `currencyCode` has: the platform's answer once the country list has
 * loaded, the compiled-in fallback before that.
 */
export function exponentFor(currencyCode: string): number {
  return exponents()[currencyCode] ?? FALLBACK_EXPONENT[currencyCode] ?? 0;
}

/** A `Money` value read as money, with its currency code — the formatter every screen calls. */
export function formatAmount(money: Money): string {
  return formatMoney(money, exponentFor(money.currency_code));
}

/** A figure an operator typed in `currencyCode`, in minor units, or `null` if it is not one. */
export function parseAmount(text: string, currencyCode: string): number | null {
  return parseMoney(text, exponentFor(currencyCode));
}

/**
 * Records the exponents a country list carries.
 *
 * Keyed on the currency rather than the country, because the exponent is a property of the currency
 * — the cent is two places wherever the dollar is spent — so two countries sharing one cannot
 * disagree, and a screen that has a `Money` has its code without knowing which country it came from.
 */
export function rememberCurrencies(countries: readonly Country[]): void {
  const next: Record<string, number> = {};
  for (const country of countries) {
    next[country.currency_code] = country.currency_exponent;
  }
  setExponents(next);
}
