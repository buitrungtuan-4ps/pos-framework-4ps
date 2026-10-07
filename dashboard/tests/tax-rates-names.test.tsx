// The tax grid's component names
// ([ADR-0168](../../docs/adr/0168-a-settled-bill-records-its-tax-components.md) decision 2), against
// a mocked cloud.
//
// A part's name is a code of two to eight capital letters and digits, starting with a letter, because
// a settled bill records it and the reports sum it under that name. What is pinned:
//
//   * a part typed in lower case is upper-cased in the input, where the operator sees it, and saved;
//   * a letter typed in the middle of a breakdown is upper-cased without throwing the caret to the
//     end of the input, so correcting a breakdown does not fight the operator on every key;
//   * a part named with words is flagged in its cell with the rule, and the grid cannot be saved;
//   * a row the cloud marks keeps its stored name and says what that costs until it is renamed, and
//     the mark gives way to the screen's own check once the breakdown is edited;
//   * the cloud's refusal of a name is said in the operator's words rather than the cloud's.

import { cleanup, fireEvent, render, screen, waitFor } from "@solidjs/testing-library";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { ApiError } from "../src/api/client";
import type { TaxRate } from "../src/api/types";
import { setLocale, t } from "../src/i18n";
import { TaxRates } from "../src/screens/TaxRates";
import { selectTenant, setActingAdmin } from "../src/state/session";

const TENANT = "01M22190WCY5PS7KCA7ET7H679";
const FOOD = "01TAXCLASSFOODAAAAAAAAAAAA";

const listTaxRates = vi.fn();
const setTaxRates = vi.fn();

vi.mock("../src/api/client", () => ({
  api: {
    listTaxClasses: () =>
      Promise.resolve([
        { tax_class_id: FOOD, tenant_id: TENANT, name: "Food", status: "active", etag: "1" },
      ]),
    listTaxRates: (...args: unknown[]) => listTaxRates(...args),
    setTaxRates: (...args: unknown[]) => setTaxRates(...args),
    configNodes: () => Promise.resolve({ current_version_id: null, nodes: [] }),
    publishTax: vi.fn(),
  },
  ApiError: class ApiError extends Error {
    constructor(
      readonly status: number,
      message: string,
      readonly canonical: string | null = null,
      readonly details: readonly { field: string; reason: string }[] = [],
    ) {
      super(message);
    }
    get isStale(): boolean {
      return this.status === 412;
    }
  },
}));

/** The rate and the breakdown inputs of the dine-in cell of the Food row. */
async function dineIn(): Promise<{ rate: HTMLInputElement; breakdown: HTMLInputElement }> {
  const cell = `Food ${t("channel.dineIn")}`;
  const rate = (await screen.findByLabelText(cell)) as HTMLInputElement;
  const breakdown = screen.getByLabelText(`${cell} ${t("taxRates.breakdown")}`) as HTMLInputElement;
  return { rate, breakdown };
}

/** A rate the grid was saved with: the dine-in cell at 5%, its parts as given. */
function saved(components: TaxRate["components"], marked: boolean): TaxRate {
  const rate = { tax_class_id: FOOD, sales_channel: "SALES_CHANNEL_DINE_IN" as const, rate_bps: 500 };
  return { ...rate, components, ...(marked ? { component_name_invalid: true } : {}) };
}

const save = () => screen.getByRole("button", { name: t("action.save") }) as HTMLButtonElement;

describe("the tax grid's component names", () => {
  beforeEach(() => {
    localStorage.clear();
    vi.clearAllMocks();
    setLocale("en");
    listTaxRates.mockResolvedValue({ value: [], etag: null });
    setTaxRates.mockResolvedValue({ value: [], etag: "2" });
    setActingAdmin({
      id: "01ADMINAAAAAAAAAAAAAAAAAAA",
      email: "admin@example.test",
      name: "Admin",
      role: "owner",
      status: "active",
    });
    selectTenant(TENANT, "Pizza 4P's India");
  });
  afterEach(() => {
    cleanup();
    setActingAdmin(null);
  });

  it("upper-cases a part typed in lower case where it is typed, and saves it", async () => {
    render(() => <TaxRates />);
    const { rate, breakdown } = await dineIn();
    fireEvent.input(rate, { target: { value: "5" } });
    fireEvent.input(breakdown, { target: { value: "cgst 2.5, sgst 2.5" } });
    expect(breakdown.value).toBe("CGST 2.5, SGST 2.5");
    expect(screen.queryByText(t("taxRates.breakdownName"))).toBeNull();

    fireEvent.click(save());
    await waitFor(() => expect(setTaxRates).toHaveBeenCalled());
    const [, rows] = setTaxRates.mock.calls[0] as [string, TaxRate[]];
    expect(rows).toEqual([
      saved(
        [
          { name: "CGST", rate_bps: 250 },
          { name: "SGST", rate_bps: 250 },
        ],
        false,
      ),
    ]);
  });

  it("keeps the caret one past a lower-case letter typed in the middle", async () => {
    render(() => <TaxRates />);
    const { breakdown } = await dineIn();
    fireEvent.input(breakdown, { target: { value: "CGST 2.5, SGT 2.5" } });
    // The operator puts the caret between the G and the T of SGT and types a lower-case s, which a
    // browser leaves as the letter typed and the caret one past it.
    breakdown.value = "CGST 2.5, SGsT 2.5";
    breakdown.setSelectionRange(13, 13);
    fireEvent.input(breakdown);
    expect(breakdown.value).toBe("CGST 2.5, SGST 2.5");
    expect([breakdown.selectionStart, breakdown.selectionEnd]).toEqual([13, 13]);
  });

  it("flags a part named with words in its cell, and will not save the grid", async () => {
    render(() => <TaxRates />);
    const { rate, breakdown } = await dineIn();
    fireEvent.input(rate, { target: { value: "5" } });
    fireEvent.input(breakdown, { target: { value: "Central GST 2.5, SGST 2.5" } });
    expect(screen.getByText(t("taxRates.breakdownName"))).toBeTruthy();
    expect(save().disabled).toBe(true);
  });

  it("marks a row saved with such a name until its breakdown is renamed", async () => {
    const stored = [
      { name: "Central GST", rate_bps: 250 },
      { name: "SGST", rate_bps: 250 },
    ];
    listTaxRates.mockResolvedValue({ value: [saved(stored, true)], etag: "1" });
    render(() => <TaxRates />);
    const { breakdown } = await dineIn();
    expect(await screen.findByText(t("taxRates.breakdownMarked"))).toBeTruthy();
    // As it was saved: nothing is rewritten but by a save the operator makes.
    expect(breakdown.value).toBe("Central GST 2.5, SGST 2.5");
    expect(save().disabled).toBe(true);

    fireEvent.input(breakdown, { target: { value: "CGST 2.5, SGST 2.5" } });
    expect(screen.queryByText(t("taxRates.breakdownMarked"))).toBeNull();
    expect(save().disabled).toBe(false);
  });

  it("says the cloud's refusal of a name in the operator's words", async () => {
    setTaxRates.mockRejectedValue(
      new ApiError(
        400,
        "a tax component of the rate for tax class 01TAXCLASSFOODAAAAAAAAAAAA on " +
          "SALES_CHANNEL_DINE_IN is not named with 2 to 8 upper-case letters and digits",
        "INVALID_ARGUMENT",
        [{ field: "rates", reason: "INVALID_FORMAT" }],
      ),
    );
    render(() => <TaxRates />);
    const { rate, breakdown } = await dineIn();
    fireEvent.input(rate, { target: { value: "5" } });
    fireEvent.input(breakdown, { target: { value: "CGST 2.5, SGST 2.5" } });
    fireEvent.click(save());
    expect(await screen.findByRole("alert")).toHaveProperty(
      "textContent",
      t("taxRates.refused.componentName"),
    );
    expect(screen.queryByText(/SALES_CHANNEL_DINE_IN/u)).toBeNull();
  });
});
