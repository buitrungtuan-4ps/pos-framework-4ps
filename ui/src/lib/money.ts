// Money is integer minor units the whole way (ADR-0028): đồng for VND, never a float. The wire shape
// mirrors `pos_proto::Money`. Formatting splits into major/minor with integer ops only, so no
// rounding error can slip in on the way to the screen.

export interface Money {
  currency_code: string;
  amount_minor: number;
}

export interface Ratio {
  numerator: number;
  denominator: number;
}

export interface Quantity {
  milli: number;
}

// Minor-unit digits per currency. Vietnam v1 is VND (none); the cloud config pack supplies the rest
// in P7, so this is the one place a new currency's scale is added.
const MINOR_DIGITS: Record<string, number> = { VND: 0, JPY: 0, USD: 2 };

export function money(currencyCode: string, amountMinor: number): Money {
  return { currency_code: currencyCode, amount_minor: amountMinor };
}

export function addMoney(a: Money, b: Money): Money {
  return { currency_code: a.currency_code, amount_minor: a.amount_minor + b.amount_minor };
}

export function zeroLike(m: Money): Money {
  return { currency_code: m.currency_code, amount_minor: 0 };
}

export function quantity(whole: number): Quantity {
  return { milli: whole * 1000 };
}

// A display string for an amount. `₫` for VND (the symbol most staff read fastest); the ISO code
// otherwise until the locale pack supplies a symbol.
export function formatMoney(m: Money): string {
  const digits = MINOR_DIGITS[m.currency_code] ?? 0;
  const negative = m.amount_minor < 0;
  const abs = Math.abs(m.amount_minor);
  const scale = 10 ** digits;
  const major = Math.trunc(abs / scale);
  const minor = abs % scale;
  const grouped = major.toLocaleString("en-US");
  const body = digits > 0 ? `${grouped}.${minor.toString().padStart(digits, "0")}` : grouped;
  const sign = negative ? "-" : "";
  if (m.currency_code === "VND") {
    return `${sign}${body}₫`;
  }
  return `${sign}${m.currency_code} ${body}`;
}

// The banknotes a cashier is most often handed, in minor units, per currency — the pay pad's
// quick-cash keys **until the store's locale syncs**.
//
// This table used to be the authority, and that was the bug ADR-0105 closes: which notes a guest
// carries is a fact about a country's cash, not about this app, so it now arrives on the `locale`
// config node with everything else the cloud publishes. A store trading in a currency nobody had
// typed in here got a till with one button, and the fix was a front-end edit and a release.
//
// It survives as the **fallback** for the window before `loadLocale` lands, which is the same
// never-blank contract as `DEFAULT_FLOOR` and `DEFAULT_CURRENCY`. A currency with no row still gets
// no keys, only the exact amount: the exact amount is always tenderable, and guessing denominations
// would put wrong buttons on a real till.
const QUICK_CASH: Record<string, readonly number[]> = {
  VND: [50_000, 100_000, 200_000],
  JPY: [1_000, 5_000, 10_000],
  INR: [10_000, 20_000, 50_000],
  USD: [2_000, 5_000, 10_000],
};

// The compiled-in keys for a currency, for the window before the store's own list has synced.
export function fallbackQuickCash(currencyCode: string): readonly number[] {
  return QUICK_CASH[currencyCode] ?? [];
}

// The quick-cash keys to draw: the store's denominations, largest-last, excluding anything below
// `atLeast` (a note that cannot cover the bill is not a tender the cashier can take).
export function quickCashFor(
  denominations: readonly number[],
  atLeast: number,
): readonly number[] {
  return denominations.filter((note) => note >= atLeast);
}

// Parse a whole-đồng figure a cashier typed into minor units. Digits only; anything else is `null`
// so the caller can refuse it rather than settle a wrong amount.
export function parseWhole(text: string, currencyCode: string): number | null {
  const cleaned = text.replace(/[\s,._]/g, "");
  if (cleaned === "" || !/^\d+$/.test(cleaned)) {
    return null;
  }
  const digits = MINOR_DIGITS[currencyCode] ?? 0;
  return Number(cleaned) * 10 ** digits;
}
