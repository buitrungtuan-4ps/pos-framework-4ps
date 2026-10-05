// The console offers a capability switch only where a release reads it (ADR-0160 decision 5).
//
// `GET /admin/capabilities` marks `tabs_enabled`, `pay_first_enabled`, `barcode_enabled` and
// `queue_number_enabled` as not offered: no tab, pay-first flow or barcode screen exists, and every
// tableless order is given a queue number whatever the switch says, so turning one on changed
// nothing at any store. These pin what the Config screen does with that:
//
//   * it draws no switch for a flag the catalogue does not offer, and neither a preset nor a publish
//     sets one, so a store's stored value stays as it is;
//   * a store that has one on is told so in one muted line;
//   * the one exception: table service excludes pay-first, so a publish that leaves tables on for a
//     store whose stored pay-first is on turns pay-first off too, and the line says so;
//   * a preset that turns on none of the offered switches, Retail, is not offered;
//   * every other switch draws and publishes as before, and a cloud older than the field has every
//     flag and preset offered, as it always had.

import { cleanup, fireEvent, render, screen, waitFor } from "@solidjs/testing-library";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { Config } from "../src/screens/Config";
import { selectStore, selectTenant } from "../src/state/session";

const TENANT = { id: "01M22190WCY5PS7KCA7ET7H679", name: "Pizza 4P's Vietnam" };
const STORE = { id: "01STOREAAAAAAAAAAAAAAAAAAA", name: "Ginza" };

const FLAGS = [
  { key: "tables_enabled", default_on: true, description: "Table service", offered: true },
  { key: "tabs_enabled", default_on: false, description: "Tabs", offered: false },
  { key: "pay_first_enabled", default_on: false, description: "Pay first", offered: false },
  { key: "barcode_enabled", default_on: false, description: "Barcode entry", offered: false },
  { key: "queue_number_enabled", default_on: false, description: "Queue numbers", offered: false },
  { key: "tips_enabled", default_on: true, description: "Tips", offered: true },
];

const CATALOGUE = {
  flags: FLAGS,
  presets: [
    { id: "full_service", keys: ["tables_enabled", "tips_enabled"] },
    { id: "counter", keys: ["pay_first_enabled", "queue_number_enabled", "tips_enabled"] },
    { id: "retail", keys: ["barcode_enabled"] },
  ],
  rules: [
    {
      id: "pay_first.excludes.tables",
      description: "pay_first_enabled cannot be on with tables_enabled",
    },
  ],
};

const HIDDEN = ["tabs_enabled", "pay_first_enabled", "barcode_enabled", "queue_number_enabled"];

const capabilityCatalogue = vi.fn();
const effectiveConfig = vi.fn();
const publishCapabilities = vi.fn();

vi.mock("../src/api/client", () => ({
  api: {
    effectiveConfig: (...args: unknown[]) => effectiveConfig(...args),
    configNodes: () => Promise.resolve({ current_version_id: null, nodes: [] }),
    configVersions: () => Promise.resolve([]),
    capabilityCatalogue: () => capabilityCatalogue(),
    publishCapabilities: (...args: unknown[]) => publishCapabilities(...args),
    previewNode: vi.fn(),
  },
  ApiError: class ApiError extends Error {},
}));

/** Mounts the screen over a store whose effective document is `document`. */
async function mount(document: Record<string, boolean>) {
  effectiveConfig.mockResolvedValue(document);
  render(() => <Config />);
  return (await screen.findByRole("checkbox", { name: "tables_enabled" })) as HTMLInputElement;
}

/** What the last capability publish sent. */
async function published(): Promise<Record<string, boolean>> {
  await waitFor(() => expect(publishCapabilities).toHaveBeenCalled());
  const calls = publishCapabilities.mock.calls;
  const [, , flags] = calls[calls.length - 1] as [string, string, Record<string, boolean>];
  return flags;
}

describe("the capability switches the console offers", () => {
  beforeEach(() => {
    localStorage.clear();
    vi.clearAllMocks();
    capabilityCatalogue.mockResolvedValue(CATALOGUE);
    publishCapabilities.mockResolvedValue({ config_version_id: "01VERSIONAAAAAAAAAAAAAAAAA" });
    selectTenant(TENANT.id, TENANT.name);
    selectStore(STORE.id, STORE.name);
  });
  afterEach(cleanup);

  it("draws no switch for a flag no release reads", async () => {
    await mount({});
    expect(screen.getByRole("checkbox", { name: "tips_enabled" })).toBeTruthy();
    for (const hidden of HIDDEN) {
      expect(screen.queryByRole("checkbox", { name: hidden })).toBeNull();
      expect(screen.queryByText(hidden)).toBeNull();
    }
    expect(screen.queryByText(/set but not used/)).toBeNull();
  });

  it("tells a store that has one on in one muted line, and no preset or publish sets it", async () => {
    await mount({ tables_enabled: false, pay_first_enabled: true, queue_number_enabled: true });
    const lines = screen.getAllByText(/set but not used by this release/);
    expect(lines).toHaveLength(1);
    expect(lines[0]?.textContent).toBe(
      "pay_first_enabled, queue_number_enabled: set but not used by this release.",
    );
    expect(screen.queryByRole("checkbox", { name: "pay_first_enabled" })).toBeNull();

    // The counter preset names pay-first and a queue number: the form sets the switch it offers.
    fireEvent.click(screen.getByRole("button", { name: "Counter" }));
    fireEvent.click(screen.getByRole("button", { name: "Publish capabilities" }));
    expect(await published()).toEqual({ tables_enabled: false, tips_enabled: true });
  });

  it("turns a stored pay-first off when table service goes on, says so, and is accepted", async () => {
    const tables = await mount({ tables_enabled: false, pay_first_enabled: true });
    expect(
      screen.getByText("pay_first_enabled: set but not used by this release."),
    ).toBeTruthy();

    fireEvent.click(tables);
    expect(
      screen.getByText("pay_first_enabled will be turned off because table service excludes it."),
    ).toBeTruthy();
    expect(screen.queryByText(/set but not used/)).toBeNull();
    expect(screen.queryByText(/Resolve these conflicts/)).toBeNull();
    fireEvent.click(screen.getByRole("button", { name: "Publish capabilities" }));
    expect(await published()).toEqual({
      tables_enabled: true,
      tips_enabled: true,
      pay_first_enabled: false,
    });
    expect((await screen.findAllByText(/Capabilities published as version/)).length).toBe(1);

    // And through the preset that always cleared it: Full service.
    cleanup();
    publishCapabilities.mockClear();
    await mount({ tables_enabled: false, pay_first_enabled: true });
    fireEvent.click(screen.getByRole("button", { name: "Full service" }));
    fireEvent.click(screen.getByRole("button", { name: "Publish capabilities" }));
    expect(await published()).toEqual({
      tables_enabled: true,
      tips_enabled: true,
      pay_first_enabled: false,
    });
  });

  it("leaves a stored pay-first unwritten while table service stays off", async () => {
    await mount({ tables_enabled: false, pay_first_enabled: true });
    fireEvent.click(screen.getByRole("checkbox", { name: "tips_enabled" }));
    fireEvent.click(screen.getByRole("button", { name: "Publish capabilities" }));
    expect(await published()).toEqual({ tables_enabled: false, tips_enabled: false });
  });

  it("does not offer a preset that turns on none of the offered switches", async () => {
    await mount({});
    expect(screen.getByRole("button", { name: "Full service" })).toBeTruthy();
    expect(screen.getByRole("button", { name: "Counter" })).toBeTruthy();
    expect(screen.queryByRole("button", { name: "Retail" })).toBeNull();
  });

  it("draws and publishes every other switch as before", async () => {
    const tables = await mount({ tables_enabled: true });
    expect(tables.checked).toBe(true);
    fireEvent.click(tables);
    fireEvent.click(screen.getByRole("checkbox", { name: "tips_enabled" }));
    fireEvent.click(screen.getByRole("button", { name: "Publish capabilities" }));
    expect(await published()).toEqual({ tables_enabled: false, tips_enabled: false });
  });

  it("offers every flag and preset where the cloud is older than the field", async () => {
    capabilityCatalogue.mockResolvedValue({
      ...CATALOGUE,
      flags: FLAGS.map(({ key, default_on, description }) => ({ key, default_on, description })),
    });
    await mount({});
    for (const flag of HIDDEN) {
      expect(screen.getByRole("checkbox", { name: flag })).toBeTruthy();
    }
    expect(screen.getByRole("button", { name: "Retail" })).toBeTruthy();
  });
});
