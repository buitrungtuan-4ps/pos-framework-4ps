// Store settings showed one store's values on every store (Wave 4 · PR-1, V15).
//
// The screen loaded the country registry and nothing else, so it opened on the framework's own
// defaults — VND, Asia/Ho_Chi_Minh, cutoff 04:00, prices excluding tax — whatever that shop had
// published. Two failures, and the second is the one that costs money: the operator was told the
// wrong thing, and a publish here rebuilds the whole `locale` node from the form, so opening the
// screen to correct an address and pressing Publish would move a Tokyo store's currency to VND and
// its cutoff to 4 a.m.
//
// Four properties are pinned, each a way the screen can go back to lying:
//
//   * a published node reaches every field, so what is shown is what the store runs;
//   * a store with nothing published says so, rather than presenting defaults as an answer;
//   * a node written by a fork — wrong types, missing keys — leaves the defaults standing instead
//     of throwing, because this console still has to draw that store;
//   * `cash_rounding_increment: null` is a published value meaning "do not round", not an absent
//     one, so it clears the field rather than leaving the previous store's increment in place;
//   * the date the line prints is the **locale node's**, not the tree's. The tree's version moves
//     whenever any node is published, so reading it made this screen claim the store's locale had
//     just been published because somebody had saved a tax rate (Wave 4 · PR-6c-4).

import { cleanup, fireEvent, render, screen, waitFor } from "@solidjs/testing-library";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { StoreSettings } from "../src/screens/StoreSettings";
import { selectStore, selectTenant } from "../src/state/session";

const TENANT = { id: "01M22190WCY5PS7KCA7ET7H679", name: "Pizza 4P's Vietnam" };
const STORE = { id: "01STOREAAAAAAAAAAAAAAAAAAA", name: "Ginza" };

const COUNTRIES = [
  {
    code: "JP",
    display_name: "Japan",
    currency_code: "JPY",
    default_language: "ja",
    decimal_separator: ".",
    group_separator: ",",
    digits_per_group: 3,
    default_retention_days: 365,
    prices_include_tax: true,
    cash_rounding_increment: null,
    cash_denominations: [1000, 5000, 10000],
  },
  {
    code: "VN",
    display_name: "Vietnam",
    currency_code: "VND",
    default_language: "vi",
    decimal_separator: ",",
    group_separator: ".",
    digits_per_group: 3,
    default_retention_days: 365,
    prices_include_tax: false,
    cash_rounding_increment: null,
    cash_denominations: [],
  },
];

/** What a Japanese store is actually running — none of it the framework default. */
const PUBLISHED = {
  locale: {
    country_code: "JP",
    currency_code: "JPY",
    timezone: "Asia/Tokyo",
    cutoff_hour: 6,
    display_language: "ja",
    prices_include_tax: true,
    cash_rounding_increment: null,
    cash_denominations: [1000, 5000],
  },
  store_profile: {
    legal_name: "Yotsuba Foods K.K.",
    trading_name: "Ginza",
    address_lines: ["1-2-3 Ginza", "Chuo-ku, Tokyo"],
    tax_registration_number: "T1234567890123",
    tax_registration_label: "登録番号",
    contact_lines: ["03-1234-5678"],
    footer_lines: ["ありがとうございました"],
  },
};

/** Mid-2023 and mid-2026, far enough apart that a formatted date names one year or the other. */
const LOCALE_AT = Date.UTC(2023, 5, 1);
const OTHER_NODE_AT = Date.UTC(2026, 5, 1);

const effectiveConfig = vi.fn();
const configNodes = vi.fn();
const publishRetention = vi.fn();

vi.mock("../src/api/client", () => ({
  api: {
    listCountries: () => Promise.resolve(COUNTRIES),
    effectiveConfig: (...args: unknown[]) => effectiveConfig(...args),
    configNodes: (...args: unknown[]) => configNodes(...args),
    publishLocale: vi.fn(),
    publishStoreProfile: vi.fn(),
    publishRetention: (...args: unknown[]) => publishRetention(...args),
  },
  ApiError: class ApiError extends Error {},
}));

/**
 * The value of the control whose label *starts with* `label`.
 *
 * A prefix rather than the whole string because the kit puts a field's hint inside its `<label>`,
 * so the accessible name of the cutoff input is its caption followed by two sentences of guidance.
 */
function field(label: string): string {
  const pattern = new RegExp("^" + label.replace(/[.*+?^${}()|[\]\\]/g, "\\$&"));
  return (screen.getByLabelText(pattern) as HTMLInputElement | HTMLTextAreaElement).value;
}

async function mount() {
  render(() => <StoreSettings />);
  await waitFor(() => expect(effectiveConfig).toHaveBeenCalled());
  // One more turn for the hydrated values to reach the controls.
  await waitFor(() => expect(screen.getByLabelText(/^Currency/)).toBeTruthy());
}

describe("store settings", () => {
  beforeEach(() => {
    localStorage.clear();
    vi.clearAllMocks();
    effectiveConfig.mockResolvedValue(PUBLISHED);
    configNodes.mockResolvedValue({
      current_version_id: "01VERSIONAAAAAAAAAAAAAAAAA",
      nodes: [
        { node: "locale", version_id: "01VERSIONAAAAAAAAAAAAAAAAA", at_ms: LOCALE_AT },
        { node: "store_profile", version_id: "01VERSIONAAAAAAAAAAAAAAAAA", at_ms: LOCALE_AT },
      ],
    });
    selectTenant(TENANT.id, TENANT.name);
    selectStore(STORE.id, STORE.name);
  });
  afterEach(cleanup);

  it("shows what the store is running, not the framework's defaults", async () => {
    await mount();
    expect(field("Currency (3-letter code)")).toBe("JPY");
    expect(field("Timezone (IANA name)")).toBe("Asia/Tokyo");
    expect(field("Business-date cutoff hour")).toBe("6");
    expect(field("Registered name")).toBe("Yotsuba Foods K.K.");
    expect(field("Registered address")).toBe("1-2-3 Ginza\nChuo-ku, Tokyo");
    expect(field("Quick-cash notes (minor units)")).toBe("1000, 5000");
    expect(screen.getByText(/Showing what this store is running now/)).toBeTruthy();
  });

  it("says so when the store has published nothing, rather than presenting defaults as an answer", async () => {
    effectiveConfig.mockResolvedValue(null);
    configNodes.mockResolvedValue({ current_version_id: null, nodes: [] });
    await mount();
    expect(screen.getByText(/Nothing published for this store yet/)).toBeTruthy();
    expect(field("Currency (3-letter code)")).toBe("VND");
  });

  it("keeps the defaults when a node holds values of the wrong shape", async () => {
    // A node written by a fork, or by a build older than a field. Drawing the store matters more
    // than being strict about it: a throw here is a screen an operator cannot open at all.
    effectiveConfig.mockResolvedValue({
      locale: { currency_code: 42, timezone: null, cutoff_hour: "six", cash_denominations: "lots" },
    });
    await mount();
    expect(field("Currency (3-letter code)")).toBe("VND");
    expect(field("Timezone (IANA name)")).toBe("Asia/Ho_Chi_Minh");
    expect(field("Business-date cutoff hour")).toBe("4");
  });

  it("treats a null rounding increment as published, because null is what 'do not round' is", async () => {
    await mount();
    expect(field("Cash rounding (minor units)")).toBe("");
  });

  it("shows the store's event retention, and 90 days when none is published (ADR-0145)", async () => {
    await mount();
    expect(field("Keep synced events for (days)")).toBe("90");

    cleanup();
    effectiveConfig.mockResolvedValue({ ...PUBLISHED, retention: { event_log_days: 180 } });
    await mount();
    expect(field("Keep synced events for (days)")).toBe("180");
  });

  it("publishes the retention as one number for this store", async () => {
    publishRetention.mockResolvedValue({ config_version_id: "01VERSIONCCCCCCCCCCCCCCCCC" });
    effectiveConfig.mockResolvedValue({ ...PUBLISHED, retention: { event_log_days: 120 } });
    await mount();
    fireEvent.click(screen.getByRole("button", { name: "Publish retention" }));
    await waitFor(() => expect(publishRetention).toHaveBeenCalledWith(TENANT.id, STORE.id, 120));
  });

  it("dates the line from the locale node, not from whatever moved the tree last", async () => {
    // The store published its locale in 2023 and a tax rate in 2026. Reading the tree's current
    // version — which is what this screen used to do — would date the locale 2026 and tell the
    // operator a node was fresh when it is three years old.
    configNodes.mockResolvedValue({
      current_version_id: "01VERSIONBBBBBBBBBBBBBBBBB",
      nodes: [
        { node: "locale", version_id: "01VERSIONAAAAAAAAAAAAAAAAA", at_ms: LOCALE_AT },
        { node: "tax", version_id: "01VERSIONBBBBBBBBBBBBBBBBB", at_ms: OTHER_NODE_AT },
      ],
    });
    await mount();
    const line = await screen.findByText(/Showing what this store is running now/);
    expect(line.textContent).toContain("2023");
    expect(line.textContent).not.toContain("2026");
  });
});
