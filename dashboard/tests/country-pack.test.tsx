// Choosing a store's country fills its country pack in (pilot U2/M3).
//
// The pilot set Country = Vietnam on Store settings and published, and the store went out with no
// cash rounding and no quick-cash notes: only a second select, "Start from a country", applied the
// pack, and nobody saw it. Pinned here:
//
//   * on a store with nothing published, choosing the country fills every pack field, the 1,000 ₫
//     rounding and the notes among them, and says which;
//   * on a store that has published, choosing it fills only what is blank and keeps — and names —
//     what the store already has that the pack would set differently, so nothing is overwritten
//     silently.

import { cleanup, fireEvent, render, screen, waitFor } from "@solidjs/testing-library";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { StoreSettings } from "../src/screens/StoreSettings";
import { fillFromPack, FRAMEWORK_DEFAULTS, type PackFields } from "../src/lib/country-pack";
import { selectStore, selectTenant } from "../src/state/session";

const TENANT = { id: "01M22190WCY5PS7KCA7ET7H679", name: "Pizza 4P's Vietnam" };
const STORE = { id: "01STOREAAAAAAAAAAAAAAAAAAA", name: "Xuân Thủy" };

/** The Vietnam pack as `countries/vn` compiles it: 1,000 ₫ rounding and the notes a guest hands over. */
const VIETNAM = {
  code: "VN",
  display_name: "Vietnam",
  currency_code: "VND",
  default_language: "vi",
  decimal_separator: ",",
  group_separator: ".",
  digits_per_group: 3,
  default_retention_days: 365,
  prices_include_tax: false,
  cash_rounding_increment: 1000,
  cash_denominations: [10000, 20000, 50000, 100000, 200000, 500000],
};

const effectiveConfig = vi.fn();
const toastOk = vi.fn();

vi.mock("../src/api/client", () => ({
  api: {
    listCountries: () => Promise.resolve([VIETNAM]),
    effectiveConfig: (...args: unknown[]) => effectiveConfig(...args),
    configNodes: () => Promise.resolve({ current_version_id: null, nodes: [] }),
    publishLocale: vi.fn(),
    publishStoreProfile: vi.fn(),
    publishRetention: vi.fn(),
  },
  ApiError: class ApiError extends Error {},
}));

vi.mock("../src/components/Toast", () => ({
  toast: { ok: (message: string) => toastOk(message), error: vi.fn() },
}));

function field(label: RegExp): string {
  return (screen.getByLabelText(label) as HTMLInputElement).value;
}

async function chooseVietnam() {
  render(() => <StoreSettings />);
  await waitFor(() => expect(screen.getByLabelText(/^Country/)).toBeTruthy());
  await waitFor(() => expect(effectiveConfig).toHaveBeenCalled());
  fireEvent.change(screen.getByLabelText(/^Country/), { target: { value: "VN" } });
}

describe("choosing the store's country", () => {
  beforeEach(() => {
    localStorage.clear();
    vi.clearAllMocks();
    selectTenant(TENANT.id, TENANT.name);
    selectStore(STORE.id, STORE.name);
  });
  afterEach(cleanup);

  it("fills the pack into a store that has published nothing, and says what it filled", async () => {
    effectiveConfig.mockResolvedValue(null);
    await chooseVietnam();
    expect(field(/^Cash rounding/)).toBe("1000");
    expect(field(/^Quick-cash notes/)).toBe("10000, 20000, 50000, 100000, 200000, 500000");
    expect(field(/^Registration label/)).toBe("MST");
    expect(toastOk).toHaveBeenCalledWith(
      expect.stringMatching(/^Filled in from Vietnam's pack: .*Cash rounding.*Quick-cash notes/),
    );
  });

  it("fills only what is blank on a published store, and names what it kept", async () => {
    // The pilot's store: Vietnam recorded, rounding and notes never filled in, and a currency
    // somebody chose on purpose.
    effectiveConfig.mockResolvedValue({
      locale: {
        country_code: "VN",
        currency_code: "USD",
        timezone: "Asia/Ho_Chi_Minh",
        cutoff_hour: 4,
        cash_rounding_increment: null,
        cash_denominations: [],
      },
    });
    await chooseVietnam();
    await waitFor(() => expect(field(/^Cash rounding/)).toBe("1000"));
    expect(field(/^Currency/)).toBe("USD");
    expect(toastOk).toHaveBeenCalledWith(
      expect.stringMatching(/^Kept what this store already has for Currency/),
    );
  });
});

describe("fillFromPack", () => {
  const pack: PackFields = {
    currency: "VND",
    displayLanguage: "vi",
    pricesIncludeTax: false,
    rounding: "1000",
    notes: "10000, 20000",
    registrationLabel: "MST",
  };

  it("leaves a field someone set, even on a store that has published nothing", () => {
    const current = { ...FRAMEWORK_DEFAULTS, rounding: "500" };
    const fill = fillFromPack(current, pack, false);
    expect(fill.next.rounding).toBe("500");
    expect(fill.kept).toEqual(["rounding"]);
    expect(fill.filled).toEqual(["displayLanguage", "notes", "registrationLabel"]);
  });

  it("treats a published value as the store's answer, even when it is the framework default", () => {
    const current = { ...FRAMEWORK_DEFAULTS, pricesIncludeTax: false };
    const fill = fillFromPack(current, { ...pack, pricesIncludeTax: true }, true);
    expect(fill.next.pricesIncludeTax).toBe(false);
    expect(fill.kept).toContain("pricesIncludeTax");
  });

  it("never blanks a field from a pack that has no value for it", () => {
    const current = { ...FRAMEWORK_DEFAULTS, rounding: "1000" };
    const fill = fillFromPack(current, { ...pack, rounding: "" }, true);
    expect(fill.next.rounding).toBe("1000");
  });
});
