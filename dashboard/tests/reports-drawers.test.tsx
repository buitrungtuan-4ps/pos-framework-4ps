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
//   * beside the expected amount is the float of the shifts that closed, not of one still open;
//   * a day that kept only the store's one drawer shows no table, and reads as it always did.

import { cleanup, render, screen, within } from "@solidjs/testing-library";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import type { AdminRole } from "../src/api/types";
import { setLocale } from "../src/i18n";
import { Reports } from "../src/screens/Reports";
import { formatAmount } from "../src/state/money";
import { selectStore, selectTenant, setActingAdmin } from "../src/state/session";
import type { CashTotals, XzReport } from "../src/api/schemas";

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
    closed_float: closed === undefined ? 0 : float,
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
      ["The store's drawer", vnd(200_000), vnd(50_000), vnd(20_000), vnd(0), vnd(200_000), vnd(230_000), vnd(230_000), vnd(0), vnd(0)],
      ["Bar till", vnd(500_000), vnd(100_000), vnd(30_000), vnd(0), vnd(500_000), vnd(1_570_000), vnd(1_560_000), vnd(0), vnd(-10_000)],
      [`A till no longer listed${GONE}`, vnd(100_000), vnd(10_000), vnd(0), vnd(0), "—", "—", "—", vnd(0), "—"],
    ]);
  });

  it("sets beside a drawer's expected amount only the float of its closed shift, not the open one's", async () => {
    // The first shift opened on 500,000 and closed expecting 1,570,000; the second opened on
    // 300,000 and is still open. Floats opened adds both; the closed float is the first's alone.
    const bar = { ...drawer(800_000, [100_000, 30_000], [1_570_000, 1_570_000]), shifts_opened: 2, closed_float: 500_000 };
    xzReport.mockResolvedValue(report(bar, { [BAR]: bar, store: drawer(0, [0, 0]) }, { [BAR]: "Bar till" }));
    render(() => <Reports />);

    const [, row] = await drawerRowsShown();
    expect(row?.slice(0, 7)).toEqual(["Bar till", vnd(800_000), vnd(100_000), vnd(30_000), vnd(0), vnd(500_000), vnd(1_570_000)]);
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
    expect(screen.queryByText("Voided after payment")).toBeNull();
  });

  // ADR-0184: the bills voided after payment, per store and never per person, and the cash each
  // drawer handed back for them.
  it("shows the bills voided after payment and the cash each drawer handed back", async () => {
    const bar = { ...drawer(500_000, [0, 0]), voided_cash: 113_000 };
    const day = report({ ...drawer(500_000, [0, 0]), voided_cash: 113_000 }, { [BAR]: bar }, {
      [BAR]: "Bar till",
    });
    day.revenue.voided_after_payment = { bills: 1, amount: 113_000 };
    xzReport.mockResolvedValue(day);
    render(() => <Reports />);

    const label = await screen.findByText("Voided after payment");
    expect(label.nextElementSibling?.textContent).toBe(`1 bill, ${vnd(113_000)}`);
    expect(screen.getByText("Cash handed back").nextElementSibling?.textContent).toBe(
      vnd(113_000),
    );
    expect((await drawerRowsShown())[0]?.[8]).toBe(vnd(113_000));
  });

  // ADR-0186: cash dropped to the safe is shown apart from what was paid out, not inside it.
  it("shows what each drawer dropped to the safe beside what it paid out", async () => {
    const bar = { ...drawer(500_000, [0, 30_000]), dropped: 1_000_000 };
    xzReport.mockResolvedValue(report(bar, { [BAR]: bar }, { [BAR]: "Bar till" }));
    render(() => <Reports />);
    const [row] = await drawerRowsShown();
    expect(row?.slice(3, 5)).toEqual([vnd(30_000), vnd(1_000_000)]);
    expect(screen.getByText("Dropped to safe")).toBeTruthy();
  });

  // ADR-0187 and UX-3: a drawer handed over at the till is one row whose cash counts once, with its
  // handover difference; one whose shifts name no handover lists each shift, not a sum.
  it("reads a handed-over drawer once and lists the shifts of one that was not", async () => {
    const shift = (drawer: string, float: number, closed: [number, number], handed_over = false) => ({
      drawer,
      handed_over,
      opening_float: float,
      paid_in: 0,
      paid_out: 0,
      closed: true,
      expected: closed[0],
      counted: closed[1],
      variance: closed[1] - closed[0],
    });
    const bar = { ...drawer(1_000_000, [0, 85_000], [2_212_000, 2_195_000]), shifts_opened: 2, shifts_closed: 2, handover_difference: -10_000 };
    const counter = { ...drawer(1_280_000, [0, 0], [3_000_000, 2_990_000]), shifts_opened: 2, shifts_closed: 2 };
    const day = report(bar, { [BAR]: bar, [GONE]: counter }, { [BAR]: "Bar till", [GONE]: "Counter" });
    day.cash.by_shift = {
      "01SHIFTA": shift(BAR, 1_000_000, [1_787_000, 1_780_000]),
      "01SHIFTB": shift(BAR, 1_770_000, [2_195_000, 2_195_000], true),
      "01SHIFTC": shift(GONE, 500_000, [1_073_000, 1_043_000]),
      "01SHIFTD": shift(GONE, 780_000, [1_927_000, 1_947_000]),
    };
    xzReport.mockResolvedValue(day);
    render(() => <Reports />);

    const rows = await drawerRowsShown();
    expect(rows.map((row) => [row[0], row[1], row[6], row[9], row[10]])).toEqual([
      ["Bar till", vnd(1_000_000), vnd(2_212_000), vnd(-10_000), vnd(-17_000)],
      ["CounterShift 1", vnd(500_000), vnd(1_073_000), vnd(0), vnd(-30_000)],
      ["CounterShift 2", vnd(780_000), vnd(1_927_000), vnd(0), vnd(20_000)],
    ]);
    expect(screen.getByText("Handover difference")).toBeTruthy();
  });
});
