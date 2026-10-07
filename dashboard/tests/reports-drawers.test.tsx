// The X/Z report's cash by drawer on the Reports screen
// ([ADR-0167](../../docs/adr/0167-a-till-has-its-own-cash-drawer.md) decision 13), against a mocked
// cloud.
//
// What is pinned is what an owner reads the table for:
//
//   * each drawer is a row under the store's totals: the store's first, then each till under the
//     name the cloud read from the store's devices, then a till no longer listed by its id;
//   * a drawer none of whose shifts has closed shows no expected amount, count or over/short, as
//     only a close carries one;
//   * a day that kept only the store's one drawer shows no table, and reads as it always did.

import { cleanup, render, screen, within } from "@solidjs/testing-library";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import type { AdminRole, CashTotals, XzReport } from "../src/api/types";
import { setLocale } from "../src/i18n";
import { Reports } from "../src/screens/Reports";
import { formatAmount } from "../src/state/money";
import { selectStore, selectTenant, setActingAdmin } from "../src/state/session";

const TENANT = "01M22190WCY5PS7KCA7ET7H679";
const STORE = "01STOREAAAAAAAAAAAAAAAAAAA";
const BAR = "01TERMINALAAAAAAAAAAAAAAAA";
const GONE = "01TERMINALBBBBBBBBBBBBBBBB";

const vnd = (amount_minor: number) => formatAmount({ amount_minor, currency_code: "VND" });

/** A drawer's day: started on `float`, paid in and out, and closed with `(expected, counted)`. */
function drawer(float: number, paid: [number, number], closed?: [number, number]): CashTotals {
  const [expected, counted] = closed ?? [0, 0];
  return {
    opening_float: float,
    paid_in: paid[0],
    paid_out: paid[1],
    shifts_opened: 1,
    shifts_closed: closed === undefined ? 0 : 1,
    expected,
    counted,
    variance: counted - expected,
  };
}

/** An X report whose cash is `by_drawer`'s, the store's totals given as `totals`. */
function report(
  totals: CashTotals,
  by_drawer: Record<string, CashTotals>,
  till_names: Record<string, string>,
): XzReport {
  const business_date = "2026-03-15";
  return {
    kind: "X",
    business_date,
    activity: { business_date, total_events: 0, by_type: {} },
    revenue: {
      business_date,
      currency_code: "VND",
      bills: 0,
      gross: 0,
      reductions: 0,
      service_charge: 0,
      tax: 0,
      net: 0,
      by_item: {},
      by_fee: {},
    },
    cash: { business_date, currency_code: "VND", ...totals, by_drawer },
    till_names,
  };
}

const xzReport = vi.fn();

vi.mock("../src/api/client", () => ({
  api: {
    dailyRollups: () => Promise.resolve([]),
    dailyRevenue: () => Promise.resolve([]),
    xzReport: (...args: unknown[]) => xzReport(...args),
  },
  ApiError: class ApiError extends Error {},
}));

/** Each row of the drawer table, as the cells read. */
async function drawerRowsShown(): Promise<string[][]> {
  const table = (await screen.findByText("Each drawer")).closest("table") as HTMLElement;
  return [...within(table).getAllByRole("row")]
    .slice(1)
    .map((row) => [...row.querySelectorAll("td")].map((cell) => cell.textContent ?? ""));
}

describe("the X/Z report's drawers", () => {
  beforeEach(() => {
    localStorage.clear();
    vi.clearAllMocks();
    setLocale("en");
    selectTenant(TENANT, "Pizza 4P's Vietnam");
    selectStore(STORE, "Bến Thành");
    setActingAdmin({
      id: "01ADMINAAAAAAAAAAAAAAAAAAA",
      email: "a@example.test",
      name: "A",
      role: "owner" satisfies AdminRole,
      status: "active",
    });
  });
  afterEach(() => {
    cleanup();
    setActingAdmin(null);
  });

  it("lists each drawer under the store's, named, and an open one with nothing expected", async () => {
    xzReport.mockResolvedValue(
      report(
        drawer(800_000, [160_000, 50_000], [1_800_000, 1_790_000]),
        {
          [GONE]: drawer(100_000, [10_000, 0]),
          store: drawer(200_000, [50_000, 20_000], [230_000, 230_000]),
          [BAR]: drawer(500_000, [100_000, 30_000], [1_570_000, 1_560_000]),
        },
        { [BAR]: "Bar till" },
      ),
    );
    render(() => <Reports />);

    expect(await drawerRowsShown()).toEqual([
      ["The store's drawer", vnd(200_000), vnd(50_000), vnd(20_000), vnd(230_000), vnd(230_000), vnd(0)],
      ["Bar till", vnd(500_000), vnd(100_000), vnd(30_000), vnd(1_570_000), vnd(1_560_000), vnd(-10_000)],
      [`A till no longer listed${GONE}`, vnd(100_000), vnd(10_000), vnd(0), "—", "—", "—"],
    ]);
  });

  it("shows a till's drawer even when it is the day's only one", async () => {
    xzReport.mockResolvedValue(
      report(drawer(500_000, [0, 0]), { [BAR]: drawer(500_000, [0, 0]) }, { [BAR]: "Bar till" }),
    );
    render(() => <Reports />);
    expect((await drawerRowsShown()).map((row) => row[0])).toEqual(["Bar till"]);
  });

  it("shows no table for a day that kept only the store's drawer", async () => {
    const totals = drawer(200_000, [50_000, 20_000], [230_000, 230_000]);
    xzReport.mockResolvedValue(report(totals, { store: totals }, {}));
    render(() => <Reports />);
    // The store's expected and counted, as the report has always shown them.
    expect(await screen.findAllByText(vnd(230_000).replace(/\s/g, " "))).toHaveLength(2);
    expect(screen.queryByText("Each drawer")).toBeNull();
  });
});
