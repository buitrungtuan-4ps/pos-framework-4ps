// The Fees screen ([ADR-0159](../../docs/adr/0159-a-fee-is-configuration.md)), against a mocked cloud.
//
// What is pinned is what an operator relies on and could not see go wrong:
//
//   * a new fee opens on the owner's defaults, so saving without touching them writes the fee the
//     owner agreed to — taxed the way its lines are, after discounts, net of tax, not waivable;
//   * the sample bill is asked for with the rule as typed, at a store it reaches, from that store's
//     own menu — and the bill drawn is the one the cloud assembled;
//   * a save writes at the scope the picker names and lists every store's answer by name, a
//     refused store with the rule it cannot apply, and publishes again to the ones that did not
//     take it;
//   * a refusal is said in the operator's words rather than the cloud's, which names ids;
//   * a wider rule is overridden at one store under its own id, so the store runs the new rule in
//     its place rather than both.

import { cleanup, fireEvent, render, screen, waitFor } from "@solidjs/testing-library";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { ApiError } from "../src/api/client";
import { setLocale } from "../src/i18n";
import { Fees } from "../src/screens/Fees";
import { selectStore, selectTenant, setActingAdmin } from "../src/state/session";

const TENANT = { id: "01M22190WCY5PS7KCA7ET7H679", name: "Pizza 4P's Vietnam" };
const BRAND = {
  brand_id: "01BRANDAAAAAAAAAAAAAAAAAAA",
  tenant_id: TENANT.id,
  name: "Pizza 4P's",
  status: "active",
  etag: "b1",
};
const [BEN_THANH, XUAN_THUY] = [
  {
    store_id: "01STOREAAAAAAAAAAAAAAAAAAA",
    tenant_id: TENANT.id,
    brand_id: BRAND.brand_id,
    name: "Bến Thành",
    status: "active",
    etag: "s1",
  },
  {
    store_id: "01STOREBBBBBBBBBBBBBBBBBBB",
    tenant_id: TENANT.id,
    brand_id: BRAND.brand_id,
    name: "Xuân Thủy",
    status: "active",
    etag: "s2",
  },
] as const;
const STANDARD = {
  tax_class_id: "01CLASSAAAAAAAAAAAAAAAAAAA",
  tenant_id: TENANT.id,
  name: "Standard",
  status: "active",
  etag: "c1",
};
const MARGHERITA = {
  menu_item_id: "01ITEMAAAAAAAAAAAAAAAAAAAA",
  tenant_id: TENANT.id,
  name: "Margherita",
  name_translations: {},
  tax_class_id: STANDARD.tax_class_id,
  item_category_id: null,
  item_subcategory_id: null,
  image_ref: null,
  status: "active",
  etag: "i1",
};

/** What a store has published: a locale and a dine-in menu with one item on it. */
const CONFIG = {
  locale: { currency_code: "VND" },
  menu: {
    channels: [
      {
        sales_channel: "SALES_CHANNEL_DINE_IN",
        catalog: {
          items: [
            {
              menu_item_id: MARGHERITA.menu_item_id,
              display_name: "Margherita",
              unit_price: { currency_code: "VND", amount_minor: 120_000 },
              tax_class_id: STANDARD.tax_class_id,
            },
          ],
        },
      },
    ],
    fallback: { items: [] },
  },
};

const vnd = (amount_minor: number) => ({ currency_code: "VND", amount_minor });

/** The bill the cloud assembles for one Margherita with a 5% service charge, taxed at 8%. */
const BILL = {
  store_id: BEN_THANH.store_id,
  sales_channel: "SALES_CHANNEL_DINE_IN",
  lines: [
    {
      menu_item_id: MARGHERITA.menu_item_id,
      display_name: "Margherita",
      quantity: 1,
      unit_price: vnd(120_000),
      line_total: vnd(120_000),
      tax_class_id: STANDARD.tax_class_id,
    },
  ],
  subtotal: vnd(120_000),
  fee_lines: [
    {
      fee_id: "01FEEPREVIEWAAAAAAAAAAAAAA",
      code: "SVC",
      display_name: "Service charge",
      amount: vnd(6_000),
      tax: vnd(480),
      class_shares: [{ tax_class_id: STANDARD.tax_class_id, amount: vnd(6_000), tax: vnd(480) }],
    },
  ],
  service_charge: vnd(6_000),
  tax_lines: [
    {
      tax_class_id: STANDARD.tax_class_id,
      taxable_base: vnd(126_000),
      rate_basis_points: 800,
      tax: vnd(10_080),
    },
  ],
  tax_total: vnd(10_080),
  rounding_adjustment: vnd(0),
  total_due: vnd(136_080),
};

/** A 5% service charge written for every store. */
const TENANT_RULE = {
  scope: "FEE_SCOPE_TENANT",
  scope_id: TENANT.id,
  rule: {
    fee_id: "01FEEAAAAAAAAAAAAAAAAAAAAA",
    code: "SVC",
    display_name: "Service charge",
    display_name_translations: {},
    kind: "FEE_KIND_PERCENT",
    rate: { numerator: 5, denominator: 100 },
    channels: [],
    item_scope: "FEE_ITEMS_ALL",
    menu_item_ids: [],
    item_category_ids: [],
    base_discounted: true,
    base_tax_inclusive: false,
    tax: "FEE_TAX_FOLLOW_LINES",
    waivable: false,
    active: true,
  },
  update_time: "2026-10-01T09:00:00Z",
  updated_by: "01ADMINAAAAAAAAAAAAAAAAAAA",
};

const listFees = vi.fn();
const effectiveFees = vi.fn();
const effectiveConfig = vi.fn();
const previewFees = vi.fn();
const createFee = vi.fn();
const putFee = vi.fn();
const deleteFee = vi.fn();
const publishFees = vi.fn();

vi.mock("../src/api/client", () => ({
  api: {
    listStores: () => Promise.resolve([BEN_THANH, XUAN_THUY]),
    listBrands: () => Promise.resolve([BRAND]),
    listItems: () => Promise.resolve([MARGHERITA]),
    listItemCategories: () => Promise.resolve([]),
    listTaxClasses: () => Promise.resolve([STANDARD]),
    listCountries: () => Promise.resolve([{ code: "VN", currency_code: "VND" }]),
    listFees: (...args: unknown[]) => listFees(...args),
    effectiveFees: (...args: unknown[]) => effectiveFees(...args),
    effectiveConfig: (...args: unknown[]) => effectiveConfig(...args),
    previewFees: (...args: unknown[]) => previewFees(...args),
    createFee: (...args: unknown[]) => createFee(...args),
    putFee: (...args: unknown[]) => putFee(...args),
    deleteFee: (...args: unknown[]) => deleteFee(...args),
    publishFees: (...args: unknown[]) => publishFees(...args),
  },
  ApiError: class ApiError extends Error {
    readonly status: number;
    readonly canonical: string | null;
    readonly details: readonly { field: string; reason: string }[];
    constructor(
      status: number,
      message: string,
      canonical: string | null = null,
      details: readonly { field: string; reason: string }[] = [],
    ) {
      super(message);
      this.status = status;
      this.canonical = canonical;
      this.details = details;
    }
  },
}));

/** The control whose accessible name starts with `label` (the kit puts a field's hint inside it). */
function control(label: string): HTMLInputElement & HTMLSelectElement {
  return screen.getByLabelText(
    new RegExp(`^${label.replace(/[.*+?^${}()|[\]\\]/g, "\\$&")}`),
  ) as HTMLInputElement & HTMLSelectElement;
}

/** The element a declared step clicks or types into — the same handle the replay uses. */
function step(action: string, value?: string): HTMLElement {
  const selector =
    value === undefined ? `[data-step="${action}"]` : `[data-step="${action}"][data-step-value="${value}"]`;
  const found = document.querySelector<HTMLElement>(selector);
  if (found === null) {
    throw new Error(`nothing on screen carries ${selector}`);
  }
  return found;
}

function type(element: HTMLElement, value: string) {
  fireEvent.input(element, { target: { value } });
}

/** Mounts the screen and opens a new fee from its header, named `label` in the console's language. */
async function openNewFee(label = "New fee") {
  render(() => <Fees />);
  const button = await screen.findByRole("button", { name: label });
  await waitFor(() => expect((button as HTMLButtonElement).disabled).toBe(false));
  fireEvent.click(button);
}

describe("the Fees screen", () => {
  beforeEach(() => {
    localStorage.clear();
    vi.clearAllMocks();
    setLocale("en");
    listFees.mockResolvedValue([]);
    effectiveFees.mockResolvedValue({ store_id: BEN_THANH.store_id, fees: [], publishable: true });
    effectiveConfig.mockResolvedValue(CONFIG);
    selectTenant(TENANT.id, TENANT.name);
    setActingAdmin({
      id: "01ADMINAAAAAAAAAAAAAAAAAAA",
      email: "admin@example.test",
      name: "Admin",
      role: "owner",
      status: "active",
    });
  });
  afterEach(() => {
    cleanup();
    setLocale("en");
    setActingAdmin(null);
  });

  it("opens a new fee on the owner's defaults", async () => {
    await openNewFee();
    expect(screen.getByRole("heading", { name: "New fee for every store" })).toBeTruthy();
    expect(step("setKind", "FEE_KIND_PERCENT").getAttribute("aria-pressed")).toBe("true");
    expect(control("Tax on the fee").value).toBe("FEE_TAX_FOLLOW_LINES");
    expect(control("A percentage of the lines").value).toBe("after");
    expect(control("At their prices").value).toBe("net");
    expect(control("Lines it counts").value).toBe("FEE_ITEMS_ALL");
    expect(screen.getByRole("switch", { name: "Waiving" }).getAttribute("aria-checked")).toBe("false");
    expect(screen.getByRole("switch", { name: "In force" }).getAttribute("aria-checked")).toBe("true");
    // Nothing typed yet, so there is nothing to save and the form says what it lacks first.
    expect((step("saveFee") as HTMLButtonElement).disabled).toBe(true);
    expect(screen.getByText("Give the fee a code to save it.")).toBeTruthy();
  });

  it("tries the rule as typed on a sample bill from the store's own menu, and writes nothing", async () => {
    previewFees.mockResolvedValue(BILL);
    selectStore(BEN_THANH.store_id, BEN_THANH.name);
    await openNewFee();
    type(step("setCode"), "SVC");
    type(step("setDisplayName"), "Service charge");
    type(control("Rate (%)"), "5");

    const show = step("previewBill") as HTMLButtonElement;
    await waitFor(() => expect(show.disabled).toBe(false));
    fireEvent.click(show);

    await waitFor(() =>
      expect(previewFees).toHaveBeenCalledWith(
        TENANT.id,
        BEN_THANH.store_id,
        "SALES_CHANNEL_DINE_IN",
        [
          {
            scope: "FEE_SCOPE_STORE",
            scope_id: BEN_THANH.store_id,
            rule: expect.objectContaining({
              code: "SVC",
              kind: "FEE_KIND_PERCENT",
              rate: { numerator: 5, denominator: 100 },
              tax: "FEE_TAX_FOLLOW_LINES",
              base_discounted: true,
              base_tax_inclusive: false,
              waivable: false,
            }),
          },
        ],
        // The first thing the store's dine-in menu sells, once: there is always something to try.
        [{ menu_item_id: MARGHERITA.menu_item_id, quantity: 1 }],
      ),
    );
    const bill = await waitFor(() => {
      const drawn = document.querySelector<HTMLElement>('[data-preview="sample-bill"]');
      expect(drawn).not.toBeNull();
      return drawn as HTMLElement;
    });
    expect(bill.textContent).toContain("Service charge");
    expect(bill.textContent).toContain("Standard at 8%");
    expect(bill.textContent).toContain("Total due");
    expect(createFee).not.toHaveBeenCalled();
    expect(putFee).not.toHaveBeenCalled();
  });

  it("saves and publishes, lists every store's answer, and publishes again to the one that refused", async () => {
    const written = { ...TENANT_RULE, rule: { ...TENANT_RULE.rule, fee_id: "01FEENEWAAAAAAAAAAAAAAAAAA" } };
    listFees.mockResolvedValueOnce([]).mockResolvedValue([written]);
    createFee.mockResolvedValue({
      fee_id: written.rule.fee_id,
      stores: [
        { store_id: BEN_THANH.store_id, outcome: "FEE_PUBLISH_APPLIED", config_version_id: "01VERSIONAAAAAAAAAAAAAAAAA" },
        {
          store_id: XUAN_THUY.store_id,
          outcome: "FEE_PUBLISH_REFUSED",
          faults: [{ fee_id: written.rule.fee_id, reason: "TAX_RATE_NOT_CONFIGURED" }],
        },
      ],
    });
    publishFees.mockResolvedValue({
      stores: [{ store_id: XUAN_THUY.store_id, outcome: "FEE_PUBLISH_APPLIED", config_version_id: "01VERSIONBBBBBBBBBBBBBBBBB" }],
    });
    await openNewFee();
    type(step("setCode"), "SVC");
    type(step("setDisplayName"), "Service charge");
    type(control("Rate (%)"), "5");
    fireEvent.click(step("saveFee"));

    await waitFor(() =>
      expect(createFee).toHaveBeenCalledWith(
        TENANT.id,
        "FEE_SCOPE_TENANT",
        TENANT.id,
        expect.objectContaining({ code: "SVC", rate: { numerator: 5, denominator: 100 } }),
      ),
    );
    // A new fee is sent without an id: the cloud mints it.
    expect(createFee.mock.calls[0]?.[3]).not.toHaveProperty("fee_id");
    expect(await screen.findByText("1 published, 0 unchanged, 1 refused, 0 failed.")).toBeTruthy();
    expect(document.querySelector('[data-outcome="fee-published"]')).not.toBeNull();
    expect(screen.getByText(BEN_THANH.name)).toBeTruthy();
    expect(screen.getByText(XUAN_THUY.name)).toBeTruthy();
    expect(await screen.findByText("SVC: the store has no rate for its tax class")).toBeTruthy();

    fireEvent.click(screen.getByRole("button", { name: "Publish again to the stores that did not take it" }));
    await waitFor(() => expect(publishFees).toHaveBeenCalledWith(TENANT.id, XUAN_THUY.store_id));
    expect(await screen.findByText("2 published, 0 unchanged, 0 refused, 0 failed.")).toBeTruthy();
    expect(publishFees).toHaveBeenCalledTimes(1);
  });

  it("says a refusal in the operator's words and keeps what was typed", async () => {
    createFee.mockRejectedValue(
      new ApiError(
        422,
        "fee 01FEE cannot apply at store 01STOREBBBBBBBBBBBBBBBBBBB: its tax class has no rate there",
        "FAILED_PRECONDITION",
        [{ field: "rule.tax_class_id", reason: "TAX_RATE_NOT_CONFIGURED" }],
      ),
    );
    await openNewFee();
    type(step("setCode"), "SVC");
    type(step("setDisplayName"), "Service charge");
    type(control("Rate (%)"), "5");
    fireEvent.click(step("saveFee"));

    expect(
      await screen.findByText(
        "A store this reaches has no rate for the fee's tax class. Set one under Tax rates and publish it, or tax the fee another way.",
      ),
    ).toBeTruthy();
    expect((step("setCode") as HTMLInputElement).value).toBe("SVC");
  });

  it("overrides a wider rule at one store under the fee's own id", async () => {
    listFees.mockResolvedValue([TENANT_RULE]);
    putFee.mockResolvedValue({
      fee_id: TENANT_RULE.rule.fee_id,
      stores: [{ store_id: BEN_THANH.store_id, outcome: "FEE_PUBLISH_APPLIED" }],
    });
    selectStore(BEN_THANH.store_id, BEN_THANH.name);
    render(() => <Fees />);
    fireEvent.click(await screen.findByRole("button", { name: "Override here" }));
    expect(screen.getByRole("heading", { name: "Override SVC for Bến Thành" })).toBeTruthy();
    type(control("Rate (%)"), "7.5");
    fireEvent.click(step("saveFee"));

    await waitFor(() =>
      expect(putFee).toHaveBeenCalledWith(
        TENANT.id,
        "FEE_SCOPE_STORE",
        BEN_THANH.store_id,
        expect.objectContaining({
          fee_id: TENANT_RULE.rule.fee_id,
          rate: { numerator: 750, denominator: 10_000 },
        }),
      ),
    );
    expect(createFee).not.toHaveBeenCalled();
  });

  it("says the same in Vietnamese", async () => {
    setLocale("vi");
    await openNewFee("Phí mới");
    expect(screen.getByRole("heading", { name: "Phí mới cho mọi cửa hàng" })).toBeTruthy();
    expect(screen.getByText("Thử trên một hóa đơn mẫu")).toBeTruthy();
    expect(screen.getByRole("button", { name: "Lưu và phát hành" })).toBeTruthy();
  });
});
