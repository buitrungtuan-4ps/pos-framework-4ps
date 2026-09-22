// The console read money in the currency's smallest unit and called that a display
// ([ADR-0135](../../docs/adr/0135-the-console-reads-money-the-way-the-till-does.md)).
//
// `formatMoney` formatted `amount_minor` directly, which is exact for the đồng and the yen and a
// factor of a hundred out for the rupee and the dollar: a store on INR read ₹261.45 of net revenue
// as `26,145 INR` on the hub headline and on every row of the sales rollup. `MoneyField` edited the
// same integer, so the price was typed `26145` — a habit that only worked because the figure beside
// it was wrong in the same direction.
//
// The tests below are written so that restoring either half fails one: the rupee cases pin the
// arithmetic, and the Vietnamese-locale cases pin the half that a `.`-and-`,` parser would have got
// backwards.

import { cleanup, fireEvent, render } from "@solidjs/testing-library";
import { createSignal } from "solid-js";
import { afterEach, beforeEach, describe, expect, it } from "vitest";

import { MoneyField } from "../src/components/ui";
import { setLocale } from "../src/i18n";
import { formatMinor, formatMoney, parseMoney } from "../src/lib/format";
import { exponentFor, formatAmount, parseAmount, rememberCurrencies } from "../src/state/money";

const COUNTRIES = [
  { code: "VN", currency_code: "VND", currency_exponent: 0 },
  { code: "JP", currency_code: "JPY", currency_exponent: 0 },
  { code: "IN", currency_code: "INR", currency_exponent: 2 },
  { code: "US", currency_code: "USD", currency_exponent: 2 },
  // A currency no fallback table names, so "learned from the platform" is distinguishable from
  // "compiled in": the fallback would answer 0 and the published answer is 3.
  { code: "BH", currency_code: "BHD", currency_exponent: 3 },
] as const;

/** The country list as the console receives it, with only the fields under test filled in. */
function countries() {
  return COUNTRIES.map((country) => ({ ...country }) as never);
}

beforeEach(() => {
  setLocale("en");
  rememberCurrencies(countries());
});

afterEach(cleanup);

describe("formatMoney", () => {
  it("reads a two-decimal currency as money, not as its minor unit", () => {
    // The defect: 26145 paise is ₹261.45, and the console showed the paise.
    expect(formatMoney({ amount_minor: 26_145, currency_code: "INR" }, 2)).toBe("261.45 INR");
  });

  it("leaves a currency with no minor unit exactly as it was", () => {
    expect(formatMoney({ amount_minor: 150_000, currency_code: "VND" }, 0)).toBe("150,000 VND");
    expect(formatMoney({ amount_minor: 1_200, currency_code: "JPY" }, 0)).toBe("1,200 JPY");
  });

  it("pads the fraction to the currency's width", () => {
    // `261.4` is not a price anybody wrote, and a trailing-zero-trimming formatter would draw it
    // where `261.40` is meant.
    expect(formatMinor(26_140, 2)).toBe("261.40");
    expect(formatMinor(26_105, 2)).toBe("261.05");
    expect(formatMinor(5, 2)).toBe("0.05");
  });

  it("carries the sign outside the grouping, for a refund figure", () => {
    expect(formatMinor(-26_145, 2)).toBe("-261.45");
    expect(formatMinor(-150_000, 0)).toBe("-150,000");
  });

  it("writes the reader's own marks, which are swapped between the two locales shipped", () => {
    expect(formatMinor(1_234_567, 2)).toBe("12,345.67");
    setLocale("vi");
    expect(formatMinor(1_234_567, 2)).toBe("12.345,67");
  });
});

describe("parseMoney", () => {
  it("reads back what the field drew, in either locale", () => {
    for (const locale of ["en", "vi"] as const) {
      setLocale(locale);
      for (const [minor, exponent] of [
        [26_145, 2],
        [150_000, 0],
        [1_234_567, 2],
        [5, 2],
      ] as const) {
        expect(parseMoney(formatMinor(minor, exponent), exponent)).toBe(minor);
      }
    }
  });

  it("accepts a bare figure, a grouped one, and one part-way typed", () => {
    expect(parseMoney("26145", 0)).toBe(26_145);
    expect(parseMoney("150,000", 0)).toBe(150_000);
    expect(parseMoney("1 049", 0)).toBe(1_049);
    // Every prefix of a valid figure has to parse, or a field cannot be typed into: the `.` arrives
    // before the digits that follow it.
    expect(parseMoney("261.", 2)).toBe(26_100);
    expect(parseMoney("261.4", 2)).toBe(26_140);
    expect(parseMoney(".45", 2)).toBe(45);
  });

  it("refuses more decimals than the currency has, rather than rounding a price", () => {
    expect(parseMoney("261.456", 2)).toBeNull();
    expect(parseMoney("261.4", 0)).toBeNull();
  });

  it("refuses what it cannot read, rather than salvaging the digits it liked", () => {
    // The old field stripped every non-digit, so `2,6.1.5` became 2615 — a price nobody typed,
    // published without a word.
    expect(parseMoney("2,6.1.5", 2)).toBeNull();
    expect(parseMoney("26o45", 2)).toBeNull();
    expect(parseMoney("-500", 2)).toBeNull();
    expect(parseMoney("", 2)).toBeNull();
    expect(parseMoney(".", 2)).toBeNull();
    expect(parseMoney("  ", 2)).toBeNull();
  });
});

describe("exponentFor", () => {
  it("answers from the platform's country list", () => {
    expect(exponentFor("INR")).toBe(2);
    expect(exponentFor("VND")).toBe(0);
    // Three places, and no fallback table names it — so this can only have come from the read.
    expect(exponentFor("BHD")).toBe(3);
  });

  it("falls back to a named table before the list has loaded, never to zero for a decimal currency", () => {
    rememberCurrencies([]);
    expect(exponentFor("INR")).toBe(2);
    expect(exponentFor("USD")).toBe(2);
    expect(exponentFor("VND")).toBe(0);
    // Unknown to both: the arithmetic-neutral answer rather than a claim.
    expect(exponentFor("BHD")).toBe(0);
  });

  it("binds the exponent to the amount's own currency, so one screen can draw two", () => {
    expect(formatAmount({ amount_minor: 26_145, currency_code: "INR" })).toBe("261.45 INR");
    expect(formatAmount({ amount_minor: 26_145, currency_code: "VND" })).toBe("26,145 VND");
    expect(parseAmount("261.45", "INR")).toBe(26_145);
    expect(parseAmount("261.45", "VND")).toBeNull();
  });
});

describe("MoneyField", () => {
  /**
   * Mounts the field inside a real parent and hands back the input plus every value it emitted.
   *
   * The signal is not scaffolding: the field is a controlled input, so what it draws depends on the
   * parent writing back what it emitted. A plain variable here would hold `props.value` at its
   * initial value and the blur case would pass without the component doing anything.
   *
   * The input is read by tag rather than by label because the currency code is a `<span>` inside the
   * same `<label>`, so the control's accessible name is "PriceINR" — correct, and not a match for
   * the visible caption.
   */
  function mount(currencyCode: string, initial: number | null) {
    const emitted: (number | null)[] = [];
    const [value, setValue] = createSignal(initial);
    const { container } = render(() => (
      <MoneyField
        label="Price"
        currencyCode={currencyCode}
        value={value()}
        onChange={(minor) => {
          setValue(minor);
          emitted.push(minor);
        }}
      />
    ));
    const input = container.querySelector("input");
    if (input === null) {
      throw new Error("the money field drew no input");
    }
    return { input, emitted };
  }

  it("emits the minor units for a price typed as a price", () => {
    const { input, emitted } = mount("INR", null);
    fireEvent.input(input, { target: { value: "261.45" } });
    expect(emitted.at(-1)).toBe(26_145);
  });

  it("holds a half-typed decimal on screen instead of redrawing over the caret", () => {
    const { input, emitted } = mount("INR", null);
    // Keystroke by keystroke, which is the case a controlled input gets wrong: after the `.` the
    // parent holds 26100, and redrawing `261.00` would put the caret past the digits still to come.
    for (const typed of ["2", "26", "261", "261.", "261.4", "261.45"]) {
      fireEvent.input(input, { target: { value: typed } });
      expect(input.value).toBe(typed);
    }
    expect(emitted.at(-1)).toBe(26_145);
  });

  it("settles on the stored figure when the operator leaves the field", () => {
    const { input } = mount("INR", null);
    fireEvent.input(input, { target: { value: "261.4" } });
    fireEvent.blur(input);
    expect(input.value).toBe("261.40");
  });

  it("is unchanged for a currency with no minor unit", () => {
    const { input, emitted } = mount("VND", 150_000);
    expect(input.value).toBe("150,000");
    fireEvent.input(input, { target: { value: "97,500" } });
    expect(emitted.at(-1)).toBe(97_500);
    // A phone keypad with no decimal key is the right one where there is no decimal to type.
    expect(input.getAttribute("inputmode")).toBe("numeric");
  });

  it("asks for a decimal keypad only where the currency has decimals", () => {
    const { input } = mount("INR", null);
    expect(input.getAttribute("inputmode")).toBe("decimal");
  });

  it("emits null for a figure it cannot read, rather than a salvaged one", () => {
    const { input, emitted } = mount("INR", null);
    fireEvent.input(input, { target: { value: "2,6.1.5" } });
    expect(emitted.at(-1)).toBeNull();
  });
});
