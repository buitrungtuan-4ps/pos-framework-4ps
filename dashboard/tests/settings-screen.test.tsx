// Shared settings, drawn from the register (ADR-0160 decisions 3 and 5).
//
// The screen has no code for any one setting, so what is pinned here is the contract that makes
// that safe:
//
//   * a register entry reaches the screen labelled from the console's own translations, in either
//     language — and an entry the console has no words for yet still appears, under its key, so a
//     cloud that has outrun the console does not lose a setting;
//   * **honour or hide**: for one store whose release is older than the setting's, the setting is
//     gone and one line says why; a store that has not reported a release is shown the setting with
//     a note, because unknown is not older; a wider scope keeps the setting and counts the stores
//     too old to honour it;
//   * a write names its scope and its id, and what comes back is shown per store, by name, with a
//     way to publish again to the ones that failed — "saved" alone would hide them;
//   * one store shows what it runs and which level that comes from;
//   * a role without `console.config.publish` is offered no write at all.

import { cleanup, fireEvent, render, screen, waitFor } from "@solidjs/testing-library";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { ApiError } from "../src/api/client";
import { Settings } from "../src/screens/Settings";
import { setLocale } from "../src/i18n";
import { selectStore, selectTenant, setActingAdmin } from "../src/state/session";

const TENANT = { id: "01M22190WCY5PS7KCA7ET7H679", name: "Pizza 4P's Vietnam" };
const BRAND = {
  brand_id: "01BRANDAAAAAAAAAAAAAAAAAAA",
  tenant_id: TENANT.id,
  name: "Pizza 4P's",
  status: "active",
  etag: "b1",
};
const [CURRENT, BEHIND, SILENT] = [
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
  {
    store_id: "01STORECCCCCCCCCCCCCCCCCCC",
    tenant_id: TENANT.id,
    brand_id: null,
    name: "Lê Thánh Tôn",
    status: "active",
    etag: "s3",
  },
] as const;
const STORES = [CURRENT, BEHIND, SILENT];
const GROUP = {
  group_id: "01GROUPAAAAAAAAAAAAAAAAAAA",
  tenant_id: TENANT.id,
  name: "Airport branches",
  status: "active",
  store_ids: [CURRENT.store_id, SILENT.store_id],
  etag: "g1",
};

const NO_SHIFT_SELLING = {
  setting_key: "shift.no_shift_selling",
  node: "shift",
  field: "no_shift_selling",
  kind: "SETTING_KIND_CHOICE",
  values: ["NO_SHIFT_SELLING_ALLOW", "NO_SHIFT_SELLING_REFUSE"],
  default: "NO_SHIFT_SELLING_ALLOW",
  preset: "NO_SHIFT_SELLING_REFUSE",
  scopes: [
    "SETTING_SCOPE_TENANT",
    "SETTING_SCOPE_BRAND",
    "SETTING_SCOPE_STORE_GROUP",
    "SETTING_SCOPE_STORE",
  ],
  since: "0.14.1",
};

/** A setting from a newer cloud, which this console ships no translation for. */
const UNTRANSLATED = {
  setting_key: "printing.receipt_on_settle",
  node: "printing",
  field: "receipt_on_settle",
  kind: "SETTING_KIND_CHOICE",
  values: ["RECEIPT_ON_SETTLE_ALWAYS", "RECEIPT_ON_SETTLE_ASK"],
  default: "RECEIPT_ON_SETTLE_ALWAYS",
  scopes: ["SETTING_SCOPE_TENANT", "SETTING_SCOPE_STORE"],
  since: "0.15.0",
};

/**
 * A whole number and a switch, in the shapes the catalogue gives them (ADR-0160 decision 9). The
 * register has neither yet, so these carry keys the console has no words for and fall back to them.
 */
const WAIT_SECONDS = {
  setting_key: "example.wait_seconds",
  node: "example",
  field: "wait_seconds",
  kind: "SETTING_KIND_INT",
  min: 0,
  max: 3600,
  unit: "SETTING_UNIT_SECONDS",
  default: 0,
  preset: 120,
  scopes: ["SETTING_SCOPE_TENANT", "SETTING_SCOPE_STORE"],
  since: "0.14.1",
};
const PRINT_ON_SETTLE = {
  setting_key: "example.print_on_settle",
  node: "example",
  field: "print_on_settle",
  kind: "SETTING_KIND_BOOL",
  default: true,
  scopes: ["SETTING_SCOPE_TENANT", "SETTING_SCOPE_STORE"],
  since: "0.14.1",
};

/** One store on the release that honours the setting, one behind it, and one that never said. */
const FLEET = [
  { store_id: CURRENT.store_id, installed_version: "0.14.1" },
  { store_id: BEHIND.store_id, installed_version: "0.14.0" },
  { store_id: SILENT.store_id, installed_version: null },
];

const settingsCatalogue = vi.fn();
const listSettingValues = vi.fn();
const effectiveSettings = vi.fn();
const putSetting = vi.fn();
const clearSetting = vi.fn();
const publishSettings = vi.fn();
const applySettingPresets = vi.fn();
const listFleet = vi.fn();

vi.mock("../src/api/client", () => ({
  api: {
    settingsCatalogue: () => settingsCatalogue(),
    listSettingValues: (...args: unknown[]) => listSettingValues(...args),
    effectiveSettings: (...args: unknown[]) => effectiveSettings(...args),
    putSetting: (...args: unknown[]) => putSetting(...args),
    clearSetting: (...args: unknown[]) => clearSetting(...args),
    publishSettings: (...args: unknown[]) => publishSettings(...args),
    applySettingPresets: (...args: unknown[]) => applySettingPresets(...args),
    listStores: () => Promise.resolve(STORES),
    listBrands: () => Promise.resolve([BRAND]),
    listStoreGroups: () => Promise.resolve([GROUP]),
    listFleet: () => listFleet(),
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

function signInAs(role: "owner" | "ops" | "viewer") {
  setActingAdmin({ id: "01ADMINAAAAAAAAAAAAAAAAAAA", email: "admin@example.test", name: "Admin", role, status: "active" });
}

async function mount() {
  render(() => <Settings />);
  await waitFor(() => expect(settingsCatalogue).toHaveBeenCalled());
}

/** The control whose accessible name starts with `label` (the kit puts a field's hint inside it). */
function control(label: string): HTMLSelectElement {
  return screen.getByLabelText(new RegExp(`^${label.replace(/[.*+?^${}()|[\]\\]/g, "\\$&")}`)) as HTMLSelectElement;
}

/** The option labels of a select, as an operator reads them. */
function optionLabels(select: HTMLSelectElement): string[] {
  return [...select.options].map((option) => option.textContent ?? "");
}

describe("shared settings", () => {
  beforeEach(() => {
    localStorage.clear();
    vi.clearAllMocks();
    setLocale("en");
    settingsCatalogue.mockResolvedValue([NO_SHIFT_SELLING]);
    listSettingValues.mockResolvedValue([]);
    effectiveSettings.mockResolvedValue([{ setting_key: NO_SHIFT_SELLING.setting_key, value: "NO_SHIFT_SELLING_ALLOW" }]);
    listFleet.mockResolvedValue(FLEET);
    selectTenant(TENANT.id, TENANT.name);
    signInAs("owner");
  });
  afterEach(() => {
    cleanup();
    setLocale("en");
    setActingAdmin(null);
  });

  it("draws a setting from the register, in the console's own words", async () => {
    await mount();
    expect(await screen.findByText("Selling with no shift open")).toBeTruthy();
    expect(
      screen.getByText(
        "Whether a till may seat a table, start a counter order or take a payment while no shift is open.",
      ),
    ).toBeTruthy();
    expect(
      screen.getByText(
        "Default: Allow · New stores: Refuse until a shift is open · Honoured from release 0.14.1",
      ),
    ).toBeTruthy();
    // Opened on every store, since no store is in context, with nothing written there yet.
    expect(optionLabels(control("Value for every store"))).toEqual([
      "Not set here",
      "Allow",
      "Refuse until a shift is open",
    ]);
  });

  it("says the same in Vietnamese", async () => {
    setLocale("vi");
    await mount();
    expect(await screen.findByText("Bán khi chưa mở ca")).toBeTruthy();
    expect(optionLabels(control("Giá trị cho mọi cửa hàng"))).toEqual([
      "Chưa đặt ở đây",
      "Cho phép",
      "Từ chối cho đến khi mở ca",
    ]);
  });

  it("still draws a setting it has no words for, under its own key", async () => {
    // A register entry and its translations are all a new setting needs; until the translations
    // ship, the key and the tokens stand in rather than the setting going missing.
    settingsCatalogue.mockResolvedValue([NO_SHIFT_SELLING, UNTRANSLATED]);
    await mount();
    expect(await screen.findByText("printing.receipt_on_settle")).toBeTruthy();
    const selects = screen.getAllByLabelText(/^Value for every store/) as HTMLSelectElement[];
    expect(selects).toHaveLength(2);
    expect(optionLabels(selects[1]!)).toEqual([
      "Not set here",
      "RECEIPT_ON_SETTLE_ALWAYS",
      "RECEIPT_ON_SETTLE_ASK",
    ]);
  });

  it("hides a setting from a store whose release does not honour it, and says why", async () => {
    selectStore(BEHIND.store_id, BEHIND.name);
    await mount();
    expect(
      await screen.findByText(
        "Selling with no shift open is hidden for Xuân Thủy: it runs release 0.14.0, and this setting is honoured from 0.14.1.",
      ),
    ).toBeTruthy();
    // Hidden means hidden: no card, and nothing to set it with.
    expect(screen.queryByRole("heading", { name: "Selling with no shift open" })).toBeNull();
    expect(screen.queryByRole("button", { name: "Save and publish" })).toBeNull();
  });

  it("shows it to a store that has not reported a release, with a note, because unknown is not older", async () => {
    selectStore(SILENT.store_id, SILENT.name);
    await mount();
    expect(await screen.findByRole("heading", { name: "Selling with no shift open" })).toBeTruthy();
    expect(
      screen.getByText(
        "Lê Thánh Tôn has not reported which release it runs, so it may not honour this yet. It is honoured from release 0.14.1.",
      ),
    ).toBeTruthy();
    expect(screen.getByRole("button", { name: "Save and publish" })).toBeTruthy();
  });

  it("keeps the setting for a wider scope and counts the stores too old to honour it", async () => {
    await mount();
    expect(await screen.findByText("This reaches 3 stores.")).toBeTruthy();
    expect(
      screen.getByText(
        "1 of 3 stores here runs a release older than 0.14.1 and ignores this until it updates.",
      ),
    ).toBeTruthy();
    expect(screen.getByText("1 of 3 stores here has not reported which release it runs.")).toBeTruthy();
  });

  it("hides nothing when the fleet cannot be read, and says the releases are unknown", async () => {
    // The release notes are what a fleet outage costs. Treating every store as older would take
    // the whole screen away from an operator because a different read failed.
    listFleet.mockRejectedValue(new ApiError(503, "the fleet is unavailable"));
    selectStore(BEHIND.store_id, BEHIND.name);
    await mount();
    expect(await screen.findByRole("heading", { name: "Selling with no shift open" })).toBeTruthy();
    expect(
      screen.getByText(
        "Could not read which release each store runs, so every store counts as not having reported one: the fleet is unavailable",
      ),
    ).toBeTruthy();
  });

  it("shows one store what it runs and which level it comes from", async () => {
    effectiveSettings.mockResolvedValue([
      {
        setting_key: NO_SHIFT_SELLING.setting_key,
        value: "NO_SHIFT_SELLING_REFUSE",
        scope: "SETTING_SCOPE_BRAND",
        scope_id: BRAND.brand_id,
      },
    ]);
    selectStore(CURRENT.store_id, CURRENT.name);
    await mount();
    expect(
      await screen.findByText(
        "Bến Thành runs “Refuse until a shift is open”, set for the brand Pizza 4P's.",
      ),
    ).toBeTruthy();
    expect(effectiveSettings).toHaveBeenCalledWith(TENANT.id, CURRENT.store_id);
  });

  it("writes a value for a brand, lists what every store did, and publishes again to the one that failed", async () => {
    putSetting.mockResolvedValue({
      stores: [
        { store_id: CURRENT.store_id, outcome: "SETTING_PUBLISH_APPLIED", config_version_id: "01VERSIONAAAAAAAAAAAAAAAAA" },
        { store_id: BEHIND.store_id, outcome: "SETTING_PUBLISH_FAILED" },
      ],
    });
    publishSettings.mockResolvedValue({
      stores: [{ store_id: BEHIND.store_id, outcome: "SETTING_PUBLISH_APPLIED", config_version_id: "01VERSIONBBBBBBBBBBBBBBBBB" }],
    });
    await mount();
    await screen.findByText("Selling with no shift open");

    fireEvent.change(control("Set for"), { target: { value: "SETTING_SCOPE_BRAND" } });
    fireEvent.click(screen.getByRole("button", { name: "Brand" }));
    fireEvent.click(screen.getByRole("option", { name: BRAND.name }));
    expect(await screen.findByText("This reaches 2 stores.")).toBeTruthy();

    fireEvent.change(control(`Value for ${BRAND.name}`), {
      target: { value: "NO_SHIFT_SELLING_REFUSE" },
    });
    fireEvent.click(screen.getByRole("button", { name: "Save and publish" }));

    await waitFor(() =>
      expect(putSetting).toHaveBeenCalledWith(
        TENANT.id,
        NO_SHIFT_SELLING.setting_key,
        "SETTING_SCOPE_BRAND",
        BRAND.brand_id,
        "NO_SHIFT_SELLING_REFUSE",
      ),
    );
    expect(await screen.findByText("1 published, 0 unchanged, 1 failed.")).toBeTruthy();
    // Every store, by name — the failure as visible as the success.
    expect(screen.getByText(CURRENT.name)).toBeTruthy();
    expect(screen.getByText(BEHIND.name)).toBeTruthy();
    expect(screen.getByText("Failed")).toBeTruthy();

    fireEvent.click(screen.getByRole("button", { name: "Publish again to the stores that failed" }));
    await waitFor(() => expect(publishSettings).toHaveBeenCalledWith(TENANT.id, BEHIND.store_id));
    expect(await screen.findByText("2 published, 0 unchanged, 0 failed.")).toBeTruthy();
    expect(publishSettings).toHaveBeenCalledTimes(1);
    expect(screen.queryByRole("button", { name: "Publish again to the stores that failed" })).toBeNull();
  });

  it("clears a value written here, at the scope it was written", async () => {
    listSettingValues.mockResolvedValue([
      {
        setting_key: NO_SHIFT_SELLING.setting_key,
        scope: "SETTING_SCOPE_TENANT",
        scope_id: TENANT.id,
        value: "NO_SHIFT_SELLING_REFUSE",
        update_time: "2026-10-01T09:00:00Z",
      },
    ]);
    clearSetting.mockResolvedValue({ stores: [] });
    await mount();
    await screen.findByText("Selling with no shift open");
    // What is written is what the control shows, and there is nothing "not set" to choose.
    const select = control("Value for every store");
    await waitFor(() => expect(select.value).toBe("NO_SHIFT_SELLING_REFUSE"));
    expect(optionLabels(select)).toEqual(["Allow", "Refuse until a shift is open"]);

    fireEvent.click(screen.getByRole("button", { name: "Clear this value" }));
    await waitFor(() =>
      expect(clearSetting).toHaveBeenCalledWith(
        TENANT.id,
        NO_SHIFT_SELLING.setting_key,
        "SETTING_SCOPE_TENANT",
        TENANT.id,
      ),
    );
  });

  it("gives one store the new-store values it does not set itself, after asking", async () => {
    applySettingPresets.mockResolvedValue({
      applied: [NO_SHIFT_SELLING.setting_key],
      stores: [{ store_id: CURRENT.store_id, outcome: "SETTING_PUBLISH_APPLIED" }],
    });
    selectStore(CURRENT.store_id, CURRENT.name);
    await mount();
    fireEvent.click(await screen.findByRole("button", { name: "Give this store the new-store values" }));
    expect(
      screen.getByText(
        "This writes for Bến Thành itself, and publishes to the store straight away: “Selling with no shift open” set to “Refuse until a shift is open”.",
      ),
    ).toBeTruthy();
    expect(applySettingPresets).not.toHaveBeenCalled();

    fireEvent.click(screen.getByRole("button", { name: "Apply" }));
    await waitFor(() => expect(applySettingPresets).toHaveBeenCalledWith(TENANT.id, CURRENT.store_id));
    expect(await screen.findByText("Gave this store 1 new-store value.")).toBeTruthy();
  });

  it("draws a whole number as a field bounded by its range and named in its unit", async () => {
    settingsCatalogue.mockResolvedValue([WAIT_SECONDS]);
    putSetting.mockResolvedValue({ stores: [] });
    await mount();
    expect(await screen.findByText("example.wait_seconds")).toBeTruthy();
    expect(
      screen.getByText(
        "Default: 0 seconds · New stores: 120 seconds · Honoured from release 0.14.1",
      ),
    ).toBeTruthy();

    const field = screen.getByLabelText(/^Value for every store/) as HTMLInputElement;
    expect(field.type).toBe("number");
    expect(field.min).toBe("0");
    expect(field.max).toBe("3600");
    expect(field.value).toBe("");
    expect(field.placeholder).toBe("Not set here");
    expect(screen.getByText("From 0 seconds to 3,600 seconds.")).toBeTruthy();

    // A number outside the range is said beside the field, and nothing is sent.
    fireEvent.input(field, { target: { value: "3601" } });
    expect(
      screen.getByText("This takes a whole number from 0 seconds to 3,600 seconds."),
    ).toBeTruthy();
    const save = screen.getByRole("button", { name: "Save and publish" }) as HTMLButtonElement;
    expect(save.disabled).toBe(true);

    fireEvent.input(field, { target: { value: "90" } });
    expect(save.disabled).toBe(false);
    fireEvent.click(save);
    await waitFor(() =>
      expect(putSetting).toHaveBeenCalledWith(
        TENANT.id,
        WAIT_SECONDS.setting_key,
        "SETTING_SCOPE_TENANT",
        TENANT.id,
        90,
      ),
    );
  });

  it("shows a whole number written here in its field, and in Vietnamese in its unit", async () => {
    settingsCatalogue.mockResolvedValue([WAIT_SECONDS]);
    listSettingValues.mockResolvedValue([
      {
        setting_key: WAIT_SECONDS.setting_key,
        scope: "SETTING_SCOPE_TENANT",
        scope_id: TENANT.id,
        value: 300,
        update_time: "2026-10-01T09:00:00Z",
      },
    ]);
    setLocale("vi");
    await mount();
    await screen.findByText("example.wait_seconds");
    expect((screen.getByLabelText(/^Giá trị cho mọi cửa hàng/) as HTMLInputElement).value).toBe(
      "300",
    );
    expect(
      screen.getByText("Mặc định: 0 giây · Cửa hàng mới: 120 giây · Có hiệu lực từ phiên bản 0.14.1"),
    ).toBeTruthy();
  });

  it("draws a switch, says which way it shows when nothing is set, and writes a boolean", async () => {
    settingsCatalogue.mockResolvedValue([PRINT_ON_SETTLE]);
    putSetting.mockResolvedValue({ stores: [] });
    await mount();
    await screen.findByText("example.print_on_settle");

    const toggle = screen.getByRole("switch", { name: "Value for every store" });
    // Nothing is written for every store, so it shows the default, and says that is what it shows.
    expect(toggle.getAttribute("aria-checked")).toBe("true");
    expect(toggle.textContent).toContain("On");
    expect(screen.getByText("Not set here: the switch shows the default.")).toBeTruthy();
    const save = screen.getByRole("button", { name: "Save and publish" }) as HTMLButtonElement;
    expect(save.disabled).toBe(true);

    fireEvent.click(toggle);
    expect(toggle.getAttribute("aria-checked")).toBe("false");
    expect(toggle.textContent).toContain("Off");
    expect(save.disabled).toBe(false);
    fireEvent.click(save);
    await waitFor(() =>
      expect(putSetting).toHaveBeenCalledWith(
        TENANT.id,
        PRINT_ON_SETTLE.setting_key,
        "SETTING_SCOPE_TENANT",
        TENANT.id,
        false,
      ),
    );
  });

  it("shows one store's switch the way the store runs it, and says so", async () => {
    settingsCatalogue.mockResolvedValue([PRINT_ON_SETTLE]);
    effectiveSettings.mockResolvedValue([
      {
        setting_key: PRINT_ON_SETTLE.setting_key,
        value: false,
        scope: "SETTING_SCOPE_TENANT",
        scope_id: TENANT.id,
      },
    ]);
    selectStore(CURRENT.store_id, CURRENT.name);
    await mount();
    expect(
      await screen.findByText("Bến Thành runs “Off”, set for every store."),
    ).toBeTruthy();
    const toggle = screen.getByRole("switch", { name: "Value for Bến Thành" });
    expect(toggle.getAttribute("aria-checked")).toBe("false");
    expect(
      screen.getByText("Not set for this store itself: the switch shows what it runs."),
    ).toBeTruthy();
  });

  it("offers a role that cannot publish nothing to write with", async () => {
    signInAs("viewer");
    await mount();
    expect(await screen.findByText("Selling with no shift open")).toBeTruthy();
    expect(
      screen.getByText(
        "Your role can read these settings but not change them, because every change publishes to stores.",
      ),
    ).toBeTruthy();
    expect(screen.queryByRole("button", { name: "Save and publish" })).toBeNull();
    expect(screen.queryByLabelText(/^Value for/)).toBeNull();
    expect(screen.getByText("Not set here")).toBeTruthy();
  });
});
