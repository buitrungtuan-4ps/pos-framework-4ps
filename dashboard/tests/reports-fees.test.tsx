// The Reports screen's fees by code ([ADR-0159](../../docs/adr/0159-a-fee-is-configuration.md)),
// against a mocked cloud.
//
// What is pinned is what an owner reads the card for and could not see go wrong:
//
//   * each fee is one row over the whole window, its days summed, under the name the latest day
//     recorded and in code order, as the export lists them;
//   * **Export CSV** asks for the fees export over the window the reads used;
//   * a role that may not read revenue is not shown the card, and the cloud is not asked for money.

import { cleanup, fireEvent, render, screen, within } from "@solidjs/testing-library";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import type { AdminRole, FeeTotal } from "../src/api/types";
import { setLocale } from "../src/i18n";
import { Reports } from "../src/screens/Reports";
import { formatAmount } from "../src/state/money";
import { selectStore, selectTenant, setActingAdmin } from "../src/state/session";

const TENANT = "01M22190WCY5PS7KCA7ET7H679";
const STORE = "01STOREAAAAAAAAAAAAAAAAAAA";

/** A trading day whose settled bills charged `by_fee`. */
function day(business_date: string, by_fee: Record<string, FeeTotal>) {
  const totals = { bills: 4, gross: 400_000, reductions: 0, service_charge: 0, tax: 32_000 };
  return { business_date, currency_code: "VND", ...totals, net: 432_000, by_item: {}, by_fee };
}

const vnd = (amount_minor: number) => formatAmount({ amount_minor, currency_code: "VND" });

const dailyRevenue = vi.fn();
const exportRevenueFeesCsv = vi.fn();

vi.mock("../src/api/client", () => ({
  api: {
    dailyRollups: () => Promise.resolve([]),
    dailyRevenue: (...args: unknown[]) => dailyRevenue(...args),
    xzReport: () => Promise.resolve(null),
    exportRevenueFeesCsv: (...args: unknown[]) => exportRevenueFeesCsv(...args),
  },
  ApiError: class ApiError extends Error {},
}));

function signIn(role: AdminRole) {
  const id = "01ADMINAAAAAAAAAAAAAAAAAAA";
  setActingAdmin({ id, email: "a@example.test", name: "A", role, status: "active" });
}

describe("the Reports screen's fees", () => {
  beforeEach(() => {
    localStorage.clear();
    vi.clearAllMocks();
    setLocale("en");
    selectTenant(TENANT, "Pizza 4P's Vietnam");
    selectStore(STORE, "Bến Thành");
  });
  afterEach(() => {
    cleanup();
    setActingAdmin(null);
  });

  it("sums each fee over the window under its code, and exports that window", async () => {
    signIn("owner");
    dailyRevenue.mockResolvedValue([
      day("2026-03-15", {
        SVC: { name: "Service", bills: 3, amount: 15_000, tax: 1_200 },
        PACK: { name: "Packaging", bills: 1, amount: 5_000, tax: 0 },
      }),
      day("2026-03-16", {}),
      day("2026-03-17", { SVC: { name: "Service charge", bills: 2, amount: 10_000, tax: 800 } }),
    ]);
    render(() => <Reports />);

    const service = (await screen.findByText("Service charge")).closest("tr") as HTMLElement;
    const cells = [...service.querySelectorAll("td")].map((cell) => cell.textContent);
    expect(cells).toEqual(["Service charge", "SVC", "5", vnd(25_000), vnd(2_000)]);
    const codes = [...(service.closest("tbody") as HTMLElement).querySelectorAll("tr")].map(
      (row) => row.querySelectorAll("td")[1]?.textContent,
    );
    expect(codes).toEqual(["PACK", "SVC"]);

    const card = screen.getByRole("heading", { name: "Fees" }).closest("section") as HTMLElement;
    fireEvent.click(within(card).getByRole("button", { name: "Export CSV" }));
    const range = { from: undefined, to: undefined };
    expect(exportRevenueFeesCsv).toHaveBeenCalledWith(TENANT, STORE, range);
  });

  it("says so when no day in the window charged a fee", async () => {
    signIn("admin");
    dailyRevenue.mockResolvedValue([day("2026-03-15", {})]);
    render(() => <Reports />);
    expect(await screen.findByText("No fees charged in this window.")).toBeTruthy();
  });

  it("shows a role that may not read revenue no fees, and asks the cloud for none", async () => {
    signIn("viewer");
    render(() => <Reports />);
    expect(await screen.findByText("Revenue is restricted to Owner and Admin roles.")).toBeTruthy();
    expect(screen.queryByRole("heading", { name: "Fees" })).toBeNull();
    expect(dailyRevenue).not.toHaveBeenCalled();
  });
});
