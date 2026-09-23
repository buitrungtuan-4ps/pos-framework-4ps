// Small presentation helpers: locale-aware counts, relative ages, and money read as money rather
// than as the integer it travels as ([ADR-0134](../../../docs/adr/0134-a-currency-says-how-many-decimals-it-has.md),
// [ADR-0135](../../../docs/adr/0135-the-console-reads-money-the-way-the-till-does.md)). Nothing here
// reaches the network or holds state: the exponent a money figure needs arrives as an argument, and
// `state/money.ts` is the one place that binds it to a currency.

import { locale } from "../i18n";
import type { Money } from "../api/types";

/** A count, grouped for the active locale (e.g. `1,234`). */
export function formatCount(value: number): string {
  return new Intl.NumberFormat(locale()).format(value);
}

/**
 * An absolute instant, as a date and a time the reader's locale writes (e.g. `23 Sep 2026, 14:05`).
 *
 * The one way the console prints a timestamp. It had three, and the third was the defect: three
 * places called `new Date(ms).toLocaleString()` with **no locale**, which renders in whatever the
 * browser is set to rather than the console the operator chose. That is not a cosmetic mismatch —
 * this console ships English and Vietnamese, which order the day and the month differently, so
 * `09/23` and `23/09` are the same instant written two ways and half the readings are wrong with
 * nothing on screen to say so.
 *
 * `dateStyle: "medium"` rather than the default, because it writes the month as a **name**. Passing
 * the locale fixes the three callers that forgot it; writing the month as a name means the reading
 * cannot be ambiguous even when the locale is one nobody here anticipated. `timeStyle: "short"`
 * drops the seconds, which no row in this console has ever needed.
 *
 * Renders in the **reader's** timezone, and says nothing about it — which is right for a
 * console-side record like "when did this key last sign in" and wrong for a store's own schedule.
 * `DateField` in the kit already draws that distinction for input, naming the store's zone beside
 * the field; the output side of it needs the store's published `locale.timezone` at the console,
 * which no API serves yet ([ADR-0125](../../../docs/adr/0125-a-release-is-one-decision-many-writes.md) §26
 * is where that matters most). Until then, no caller here can claim a zone it does not have.
 *
 * A formatter per call, matching `formatCount` and `formatRelativeAge` beside it. Reusing one
 * across calls would mean keying a cache by locale for a saving nobody has measured, at a few rows
 * a render rather than a few hundred a keystroke.
 *
 * A value `Intl` refuses falls back to the raw number rather than throwing. That guard came from
 * the copy in `MySessions`, whose own note is the reason to keep it: a malformed row should not
 * blank the table it appears in. It costs nothing on the values that are fine.
 */
export function formatInstant(atMs: number): string {
  try {
    return new Intl.DateTimeFormat(locale(), { dateStyle: "medium", timeStyle: "short" }).format(
      atMs,
    );
  } catch {
    return String(atMs);
  }
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
 * The marks the reader's locale writes numbers with: what groups the thousands, and what separates
 * the fraction. Read from `Intl` rather than listed here, so a locale added to the console gets the
 * right pair without anyone remembering a table.
 *
 * Both halves matter, and they are *swapped* between the two locales this console ships: English
 * writes `1,234.5`, Vietnamese writes `1.234,5`. A parser that assumed the English pair would read a
 * Vietnamese operator's `1.234` as one and a bit.
 *
 * This is the reader's locale, not the store's published `number_format`, which nothing reads yet —
 * ADR-0135 leaves typography to its own record. What matters here is only that the two directions
 * agree: whatever `formatMinor` draws, `parseMoney` reads back.
 */
function separators(): { group: string; decimal: string } {
  const parts = new Intl.NumberFormat(locale()).formatToParts(1234.5);
  return {
    group: parts.find((part) => part.type === "group")?.value ?? ",",
    decimal: parts.find((part) => part.type === "decimal")?.value ?? ".",
  };
}

/**
 * An integer amount in a currency's smallest unit, read as money, with no currency code attached
 * (e.g. `150,000`, `1,049.00`). `exponent` is how many decimal places that currency has.
 *
 * Bare of the code because a form field shows the code as its own adornment and must not repeat it
 * inside the input; `formatMoney` is the one that appends it.
 *
 * Integer arithmetic on the way out, as everywhere money is handled: the fraction is a remainder,
 * never a float. Padded to the currency's width, so `1,049.5` — which is not a price anyone wrote —
 * cannot appear where `1,049.50` is meant.
 */
export function formatMinor(amountMinor: number, exponent: number): string {
  const negative = amountMinor < 0;
  const absolute = Math.abs(amountMinor);
  const scale = 10 ** exponent;
  const major = Math.trunc(absolute / scale);
  const minor = absolute % scale;
  const grouped = new Intl.NumberFormat(locale()).format(major);
  const sign = negative ? "-" : "";
  if (exponent === 0) {
    return `${sign}${grouped}`;
  }
  return `${sign}${grouped}${separators().decimal}${String(minor).padStart(exponent, "0")}`;
}

/**
 * An integer `Money` value read as money, with its currency code appended (e.g. `150,000 VND`,
 * `1,049.00 INR`).
 *
 * This used to format `amount_minor` directly and said why: *"the exponent is a locale-pack property
 * this presentation layer does not carry."* It carries one now (ADR-0135) — and what it printed in
 * the meantime was not an abstention but a wrong figure: a store on INR read ₹261.45 of net revenue
 * as `26,145 INR`, on the headline card the business is judged from.
 *
 * `exponent` is an argument rather than a lookup, so this stays a pure function of its inputs and
 * the state module stays the one place a fetched value lives — the same split the till made in
 * ADR-0134. Screens call `formatAmount` from `state/money.ts`, which binds the exponent for the
 * amount's own currency; nobody has to remember to pass the right number, and nobody can pass none.
 */
export function formatMoney(money: Money, exponent: number): string {
  return `${formatMinor(money.amount_minor, exponent)} ${money.currency_code}`;
}

/**
 * A figure an operator typed, in minor units, or `null` when it is not one the console can send.
 *
 * The inverse of `formatMinor`, and the direction that matters more: a wrong exponent when *reading*
 * merely looks wrong, while a wrong exponent when *writing* authors a different price.
 *
 * Whitespace and the locale's grouping mark are noise and are dropped — an operator who pastes back
 * the `150,000` the field drew means 150000. What is left must be digits and at most one decimal
 * mark; anything else is refused rather than salvaged, because salvaging a price is how `2,615`
 * becomes a price nobody typed.
 *
 * More decimals than the currency has is refused for the same reason. Rounding `261.456` to `261.46`
 * would be a silent decision about somebody's menu.
 */
export function parseMoney(text: string, exponent: number): number | null {
  const { group, decimal } = separators();
  // `split`/`join` rather than a regex: the separators are literal characters handed over by `Intl`,
  // and compiling them into a pattern would need escaping in exchange for nothing.
  const bare = text.replace(/\s/g, "").split(group).join("");
  const [major = "", fraction = "", ...extra] = bare.split(decimal).join(".").split(".");
  if (extra.length > 0 || !/^\d*$/.test(major) || !/^\d*$/.test(fraction)) {
    return null;
  }
  if (major === "" && fraction === "") {
    return null;
  }
  if (fraction.length > exponent) {
    return null;
  }
  const scaled = `${major === "" ? "0" : major}${fraction.padEnd(exponent, "0")}`;
  const value = Number(scaled);
  return Number.isSafeInteger(value) ? value : null;
}

/**
 * A person's initials, for the account avatar (Wave 3 · Stage 6).
 *
 * The first character of the first and last word, so "Nguyễn Thị Hương" reads NH rather than NT. A
 * single word gives one letter — two letters from one word would be inventing an initial the person
 * does not have, and a Vietnamese given name is often the one word here.
 *
 * `Array.from` rather than indexing, because a name may begin with a character outside the basic
 * plane, where `name[0]` is half a surrogate pair and renders as a replacement glyph. `toLocaleUpperCase`
 * rather than `toUpperCase`, because case is locale-dependent (Turkish dotless ı is the standard
 * example) and this is the operator's own name.
 *
 * A name with no letters at all — empty, or whitespace — gives an empty string. The caller decides
 * what to draw instead; an avatar is not the place to guess.
 */
export function initials(name: string): string {
  const words = name.trim().split(/\s+/).filter((word) => word.length > 0);
  const first = words[0];
  if (first === undefined) {
    return "";
  }
  const last = words[words.length - 1] as string;
  const letters = words.length === 1 ? [first] : [first, last];
  return letters
    .map((word) => Array.from(word)[0] ?? "")
    .join("")
    .toLocaleUpperCase(locale());
}
