// The Reports screen's tax by component
// ([ADR-0168](../../docs/adr/0168-a-settled-bill-records-its-tax-components.md) decision 5), against a
// mocked cloud.
//
// What is pinned is what an owner reads the card for, and the one way it differs from the Fees card:
//
//   * each component name and rate is one row over the whole window, its days' tax summed, in order
//     of name and then rate as the export lists them, with the rate as a percent and no total row;
//   * **Export CSV** asks for the tax export over the window the reads used;
//   * a window that recorded no component, as every Vietnamese and Japanese one, shows no card.

import { cleanup, fireEvent, render, screen, within } from "@solidjs/testing-library";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import type { AdminRole, TaxComponentTotal } from "../src/api/types";
import { setLocale } from "../src/i18n";
import { Reports } from "../src/screens/Reports";
import { formatAmount } from "../src/state/money";
import { selectStore, selectTenant, setActingAdmin } from "../src/state/session";

const TENANT = "01M22190WCY5PS7KCA7ET7H679";
const STORE = "01STOREAAAAAAAAAAAAAAAAAAA";

/** A trading day in `currency_code`, recording `by_tax_component` where it recorded any. */
function day(business_date: string, currency_code: string, by_tax_component?: TaxComponentTotal[]) {
  const totals = { bills: 4, gross: 400_000, reductions: 0, service_charge: 0, tax: 20_000 };
  const split = by_tax_component === undefined ? {} : { by_tax_component };
  return { business_date, currency_code, ...totals, net: 420_000, by_item: {}, by_fee: {}, ...split };
}

/** One entry of a day's tax by component. */
function part(component_name: string, rate_basis_points: number, tax: number): TaxComponentTotal {
  return { component_name, rate_basis_points, tax };
}

const inr = (amount_minor: number) => formatAmount({ amount_minor, currency_code: "INR" });

const dailyRevenue = vi.fn();
const exportRevenueTaxCsv = vi.fn();

vi.mock("../src/api/client", () => ({
  api: {
    dailyRollups: () => Promise.resolve([]),
    dailyRevenue: (...args: unknown[]) => dailyRevenue(...args),
    xzReport: () => Promise.resolve(null),
    exportRevenueTaxCsv: (...args: unknown[]) => exportRevenueTaxCsv(...args),
  },
  ApiError: class ApiError extends Error {},
}));

function signIn(role: AdminRole) {
  const id = "01ADMINAAAAAAAAAAAAAAAAAAA";
  setActingAdmin({ id, email: "a@example.test", name: "A", role, status: "active" });
}

describe("the Reports screen's tax by component", () => {
  beforeEach(() => {
    localStorage.clear();
    vi.clearAllMocks();
    setLocale("en");
    selectTenant(TENANT, "Pizza 4P's India");
    selectStore(STORE, "Bengaluru");
  });
  afterEach(() => {
    cleanup();
    setActingAdmin(null);
  });

  it("sums each name and rate over the window in order, and exports that window", async () => {
    signIn("owner");
    dailyRevenue.mockResolvedValue([
      day("2026-03-15", "INR", [part("CGST", 900, 900), part("SGST", 900, 900)]),
      // A day before its store recorded components.
      day("2026-03-16", "INR"),
      day("2026-03-17", "INR", [part("CGST", 250, 2_500), part("SGST", 250, 2_500)]),
      day("2026-03-18", "INR", [part("CGST", 250, 2_500), part("SGST", 250, 2_500)]),
    ]);
    render(() => <Reports />);

    const heading = await screen.findByRole("heading", { name: "Tax by component" });
    const card = heading.closest("section") as HTMLElement;
    const rows = [...card.querySelectorAll("tbody tr")].map((row) =>
      [...row.querySelectorAll("td")].map((cell) => cell.textContent),
    );
    expect(rows).toEqual([
      ["CGST", "2.5%", inr(5_000)],
      ["CGST", "9%", inr(900)],
      ["SGST", "2.5%", inr(5_000)],
      ["SGST", "9%", inr(900)],
    ]);
    expect(
      within(card).getByText(
        "A bill settled before its store recorded tax components counts in the day's tax but " +
          "under no component, so these rows can add up to less than the tax.",
      ),
    ).toBeTruthy();

    fireEvent.click(within(card).getByRole("button", { name: "Export CSV" }));
    const range = { from: undefined, to: undefined };
    expect(exportRevenueTaxCsv).toHaveBeenCalledWith(TENANT, STORE, range);
  });

  it("shows no card for a window that recorded no component", async () => {
    signIn("admin");
    dailyRevenue.mockResolvedValue([day("2026-03-15", "VND"), day("2026-03-16", "VND")]);
    render(() => <Reports />);
    // The window has been read: its fees say there were none.
    expect(await screen.findByText("No fees charged in this window.")).toBeTruthy();
    expect(screen.queryByRole("heading", { name: "Tax by component" })).toBeNull();
  });
});
