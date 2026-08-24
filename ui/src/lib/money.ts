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
