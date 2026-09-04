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

// The banknotes a cashier is most often handed, in minor units, per currency — the quick-cash keys
// on the pay pad.
//
// Note values are a property of a currency, not of the app, which is why they live beside
// `MINOR_DIGITS` rather than in the Pay screen. The screen hardcoded VND's three notes until roadmap
// **E5**, so a store on any other currency was offered buttons for amounts its guests cannot hand
// over.
//
// A currency with no entry gets **no** quick-cash keys, only the exact-amount one. That is the
// honest answer rather than a guess: the exact amount is always tenderable, and inventing
// denominations for a currency nobody has entered here would put wrong buttons on a real till. A
// fork adds its own row.
const QUICK_CASH: Record<string, readonly number[]> = {
  VND: [50_000, 100_000, 200_000],
  JPY: [1_000, 5_000, 10_000],
  USD: [2_000, 5_000, 10_000],
};

// The quick-cash denominations for a currency, largest-last, excluding anything below `atLeast`
// (a note that cannot cover the bill is not a tender the cashier can take).
export function quickCashFor(currencyCode: string, atLeast: number): readonly number[] {
  return (QUICK_CASH[currencyCode] ?? []).filter((note) => note >= atLeast);
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
