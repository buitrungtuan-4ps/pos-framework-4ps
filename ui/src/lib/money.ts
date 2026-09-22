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

// How many decimal places a currency has, for the window before the store's own answer syncs
// ([ADR-0134](../../../docs/adr/0134-a-currency-says-how-many-decimals-it-has.md)).
//
// This was the authority, and that was the bug. It read `MINOR_DIGITS[code] ?? 0`, and the `?? 0`
// was right for the đồng and the yen — the two currencies this app started with — and silently
// wrong for the rupee, whose pack has shipped since ADR-0105. ₹261.45 drew as `INR 26,145`, and
// `parseWhole` put a cashier's typed discount out by a hundred in the other direction.
//
// The store's exponent now arrives on `GET /api/locale` beside its currency, and this is only the
// never-blank fallback, the same contract `fallbackQuickCash` and `DEFAULT_FLOOR` keep. It is
// consulted for exactly one currency in practice: before the locale read lands, `storeCurrency()`
// is `DEFAULT_CURRENCY`, and a store that *has* synced published the exponent in the same node as
// the currency — so the two are never out of step after boot.
//
// A currency in neither the published node nor this table gets `0`. That is the arithmetic-neutral
// answer rather than a claim, and it is unreachable from a synced store: the node that names an
// unknown currency names its exponent too.
const FALLBACK_EXPONENT: Record<string, number> = { VND: 0, JPY: 0, INR: 2, USD: 2 };

// The compiled-in exponent for a currency, for the window before the store's own has synced.
export function fallbackExponent(currencyCode: string): number {
  return FALLBACK_EXPONENT[currencyCode] ?? 0;
}

export function money(currencyCode: string, amountMinor: number): Money {
  return { currency_code: currencyCode, amount_minor: amountMinor };
}

export function addMoney(a: Money, b: Money): Money {
  return { currency_code: a.currency_code, amount_minor: a.amount_minor + b.amount_minor };
}

export function zeroLike(m: Money): Money {
  return { currency_code: m.currency_code, amount_minor: 0 };
}

// A percentage of an amount, in **whole** minor units.
//
// Not a style preference. `Money.amount_minor` is an `i64` on the wire, and the edge's deserializer
// refuses a fraction outright — a till that sent one got back a type error and showed the cashier a
// generic store error with the guest standing there. JavaScript has a single number type, so
// "integer" here means the value that *leaves* this function is one: the division is rounded rather
// than left to produce 15236.65.
//
// Half away from zero, which is what `Money::round_to_increment` does on the edge, so a figure this
// screen suggests and a figure the edge would have computed agree rather than differing by a unit.
export function percentOf(amountMinor: number, percent: number): number {
  const negative = amountMinor < 0;
  const scaled = Math.abs(amountMinor) * percent;
  const rounded = Math.trunc((scaled * 2 + 100) / 200);
  return negative ? -rounded : rounded;
}

// Snaps an amount to the nearest multiple of `increment`, halves away from zero.
//
// The mirror of `Money::round_to_increment` on the edge. A fact about a country's **coinage**, not
// about its tax: Vietnam's smallest note is 1,000 đồng and India's smallest coin is the rupee, so an
// amount off the increment is one nobody can hand over. An `increment` of zero or less means the
// country rounds nothing — Japan, where the 1-yen coin circulates — and the amount passes through.
//
// The halving is written as a doubled comparison rather than `increment / 2`, so an odd increment
// cannot round its own midpoint on a half unit that then multiplies back up.
export function roundToIncrement(amountMinor: number, increment: number): number {
  if (increment <= 0) {
    return amountMinor;
  }
  const negative = amountMinor < 0;
  const steps = Math.trunc((Math.abs(amountMinor) * 2 + increment) / (increment * 2));
  const rounded = steps * increment;
  return negative ? -rounded : rounded;
}

export function quantity(whole: number): Quantity {
  return { milli: whole * 1000 };
}

// How a country writes a number: the mark between the integer and the fraction, the mark between
// groups of digits, and how many digits go in a group. The wire shape mirrors
// `pos_proto::locale::NumberFormat`.
export interface NumberFormat {
  decimal_separator: string;
  group_separator: string;
  digits_per_group: number;
}

// How this store writes money: how many decimals its currency has (ADR-0134) and what marks its
// country groups and points a figure with (ADR-0136).
//
// One value rather than two arguments, mirroring `MoneyStyle` in `crates/pos-edge/src/printing.rs`
// so the screen and the paper are legibly the same decision. It also stops a caller pairing one
// store's exponent with another store's marks.
export interface MoneyStyle {
  exponent: number;
  format: NumberFormat;
}

// The marks this app uses until a store's own have synced — the common `1,234.50`, and deliberately
// not Vietnam's, for the reason `FALLBACK_EXPONENT` is a table of named currencies rather than a
// guess: the till boots before it knows what country it is in, and the widespread convention is the
// one least likely to be wrong on a screen nobody has configured yet.
//
// `pos-proto`'s own `NumberFormat::default` is the same triple, and its test says so by name.
export const FALLBACK_NUMBER_FORMAT: NumberFormat = {
  decimal_separator: ".",
  group_separator: ",",
  digits_per_group: 3,
};

// `1234567` as `1,234,567`, or as `1.234.567` where that is how the country writes it.
//
// Written out rather than left to `toLocaleString`, which groups by the *reader's* locale and can
// only be told a language — not a group size. It is the twin of `group_digits` in
// `crates/pos-edge/src/printing.rs`, down to the zero guard: a group of no digits would loop
// forever, and the edge refuses to apply a published format carrying one, but the guard is written
// rather than assumed.
//
// India writes `12,34,567` — three digits then pairs — which a single group size cannot express, so
// an Indian till reads `1,234,567` (ADR-0105 records the gap; ADR-0136 leaves it open).
export function groupDigits(value: number, format: NumberFormat): string {
  const digits = Math.trunc(Math.abs(value)).toString();
  const size = Math.max(1, Math.trunc(format.digits_per_group));
  let grouped = "";
  for (let index = 0; index < digits.length; index += 1) {
    if (index > 0 && (digits.length - index) % size === 0) {
      grouped += format.group_separator;
    }
    grouped += digits[index];
  }
  return grouped;
}

// A display string for an amount. `₫` for VND (the symbol most staff read fastest); the ISO code
// otherwise until the locale pack supplies a symbol.
//
// `money` is supplied rather than looked up, so this stays a pure function of its arguments and the
// store stays the one place a published value lives — the shape every other locale fact in this app
// has. Callers in screens use `formatAmount` from the store, which binds it; nobody has to remember
// to pass the right style, and nobody *can* pass none.
//
// The marks used to be `en-US`'s, for every store in every country, while the store's own were
// published and read by nothing. A Vietnamese cashier now reads `1.234.567₫` — the spelling on the
// receipt in their hand (ADR-0136).
export function formatMoney(m: Money, money: MoneyStyle): string {
  const digits = money.exponent;
  const negative = m.amount_minor < 0;
  const abs = Math.abs(m.amount_minor);
  const scale = 10 ** digits;
  const major = Math.trunc(abs / scale);
  const minor = abs % scale;
  const grouped = groupDigits(major, money.format);
  const point = money.format.decimal_separator;
  const body =
    digits > 0 ? `${grouped}${point}${minor.toString().padStart(digits, "0")}` : grouped;
  const sign = negative ? "-" : "";
  if (m.currency_code === "VND") {
    return `${sign}${body}₫`;
  }
  return `${sign}${m.currency_code} ${body}`;
}

// How many, for a ticket row. Thousandths on the wire so a half or a weighed item can be expressed
// (`pos_proto::Quantity`), and integer ops only on the way out, for the reason money uses them: a
// float here would eventually render 2.9999999 beside a price.
//
// A whole number prints bare — "3", not "3.000" — because that is what almost every line is and the
// zeros are noise on a screen read at arm's length. A fraction prints only the digits it has.
export function formatQuantity(milli: number): string {
  const negative = milli < 0;
  const abs = Math.abs(milli);
  const whole = Math.trunc(abs / 1000);
  const fraction = abs % 1000;
  const sign = negative ? "-" : "";
  if (fraction === 0) {
    return `${sign}${whole}`;
  }
  return `${sign}${whole}.${fraction.toString().padStart(3, "0").replace(/0+$/, "")}`;
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
export function parseWhole(text: string, exponent: number): number | null {
  const cleaned = text.replace(/[\s,._]/g, "");
  if (cleaned === "" || !/^\d+$/.test(cleaned)) {
    return null;
  }
  return Number(cleaned) * 10 ** exponent;
}
