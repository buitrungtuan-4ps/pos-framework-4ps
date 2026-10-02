// QR ordering is one switch (ADR-0160 decision 5).
//
// The capability flag `qr_ordering_enabled` decides whether a store takes a guest's QR order, and the
// cloud makes `qr.enabled` and the QR sales channel follow it. This console offers it in one place,
// and these pin the ways it could go back to offering three:
//
//   * Channels & payments shows the switch as the store's effective document has it: off when the
//     document does not set it, whatever `qr.enabled` says, because off is what the edge and the
//     guest intake read then. It publishes it as the capability flag it is.
//   * The QR guardrails publish no longer sends `enabled`, which the cloud would write as the switch.
//   * The channels card's QR box shows the switch rather than being a second one.
//   * The Config screen's capability form leaves the switch out, so a preset, which names no QR
//     flag, cannot switch a store's QR ordering off.

import { cleanup, fireEvent, render, screen, waitFor } from "@solidjs/testing-library";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { Channels } from "../src/screens/Channels";
import { Config } from "../src/screens/Config";
import { selectStore, selectTenant } from "../src/state/session";

const TENANT = { id: "01M22190WCY5PS7KCA7ET7H679", name: "Pizza 4P's Vietnam" };
const STORE = { id: "01STOREAAAAAAAAAAAAAAAAAAA", name: "Ginza" };

const CATALOGUE = {
  flags: [
    { key: "tables_enabled", default_on: true, description: "Table service" },
    { key: "pay_first_enabled", default_on: false, description: "Pay first" },
    { key: "qr_ordering_enabled", default_on: false, description: "Guest QR ordering" },
  ],
  presets: [{ id: "full_service", keys: ["tables_enabled"] }],
  rules: [],
};

/** A `qr` node from before the switch, which still says `enabled: true`. */
const GUARDRAILS = {
  enabled: true,
  staff_confirmation_required: true,
  per_table_limit: 10,
  rate_window_secs: 60,
  business_hours: null,
};

const effectiveConfig = vi.fn();
const publishCapabilities = vi.fn();
const publishQrGuardrails = vi.fn();

vi.mock("../src/api/client", () => ({
  api: {
    readChannels: () =>
      Promise.resolve({ enabled: ["SALES_CHANNEL_DINE_IN", "SALES_CHANNEL_QR"] }),
    readTender: () => Promise.resolve(null),
    readQrGuardrails: () => Promise.resolve(GUARDRAILS),
    readVendorPolicies: () => Promise.resolve(null),
    readOrigins: () => Promise.resolve(null),
    effectiveConfig: (...args: unknown[]) => effectiveConfig(...args),
    configNodes: () => Promise.resolve({ current_version_id: null, nodes: [] }),
    configVersions: () => Promise.resolve([]),
    capabilityCatalogue: () => Promise.resolve(CATALOGUE),
    publishCapabilities: (...args: unknown[]) => publishCapabilities(...args),
    publishQrGuardrails: (...args: unknown[]) => publishQrGuardrails(...args),
    previewNode: vi.fn(),
  },
  ApiError: class ApiError extends Error {},
}));

/** The QR ordering switch, once the screen has read the store. */
async function qrSwitch(): Promise<HTMLElement> {
  await waitFor(() => expect(effectiveConfig).toHaveBeenCalled());
  return screen.findByRole("switch", { name: "QR ordering" });
}

describe("QR ordering's one switch", () => {
  beforeEach(() => {
    localStorage.clear();
    vi.clearAllMocks();
    publishCapabilities.mockResolvedValue({ config_version_id: "01VERSIONAAAAAAAAAAAAAAAAA" });
    publishQrGuardrails.mockResolvedValue({ config_version_id: "01VERSIONBBBBBBBBBBBBBBBBB" });
    selectTenant(TENANT.id, TENANT.name);
    selectStore(STORE.id, STORE.name);
  });
  afterEach(cleanup);

  it("shows the switch as the store runs it, and off where only qr.enabled says on", async () => {
    effectiveConfig.mockResolvedValue({ qr: GUARDRAILS });
    render(() => <Channels />);
    expect((await qrSwitch()).getAttribute("aria-checked")).toBe("false");

    cleanup();
    effectiveConfig.mockResolvedValue({ qr_ordering_enabled: true, qr: GUARDRAILS });
    render(() => <Channels />);
    expect((await qrSwitch()).getAttribute("aria-checked")).toBe("true");
  });

  it("publishes the switch as the capability flag it is", async () => {
    effectiveConfig.mockResolvedValue({ qr_ordering_enabled: true });
    render(() => <Channels />);
    fireEvent.click(await qrSwitch());
    fireEvent.click(screen.getByRole("button", { name: "Publish QR ordering" }));
    await waitFor(() =>
      expect(publishCapabilities).toHaveBeenCalledWith(TENANT.id, STORE.id, {
        qr_ordering_enabled: false,
      }),
    );
  });

  it("publishes the guardrails without enabled, which would be the switch", async () => {
    effectiveConfig.mockResolvedValue({ qr_ordering_enabled: true });
    render(() => <Channels />);
    await qrSwitch();
    fireEvent.click(screen.getByRole("button", { name: "Publish QR guardrails" }));
    await waitFor(() => expect(publishQrGuardrails).toHaveBeenCalled());
    const [, , guardrails] = publishQrGuardrails.mock.calls[0] as [string, string, object];
    expect(guardrails).not.toHaveProperty("enabled");
    expect(guardrails).toMatchObject({ staff_confirmation_required: true, per_table_limit: 10 });
  });

  it("shows the switch in the channels card rather than offering a second one", async () => {
    effectiveConfig.mockResolvedValue({});
    render(() => <Channels />);
    await qrSwitch();
    // The channels card draws first; the second "QR" is the tender card's QR payment method.
    const [channelBox, tenderBox] = screen.getAllByRole("checkbox", {
      name: "QR",
    }) as HTMLInputElement[];
    expect(channelBox?.disabled).toBe(true);
    expect(channelBox?.checked).toBe(false);
    expect(tenderBox?.disabled).toBe(false);
    expect((screen.getByRole("checkbox", { name: "Dine-in" }) as HTMLInputElement).disabled).toBe(
      false,
    );
  });

  it("leaves the switch out of the Config screen's capability form and its presets", async () => {
    effectiveConfig.mockResolvedValue({ qr_ordering_enabled: true, tables_enabled: true });
    render(() => <Config />);
    await screen.findByText("tables_enabled");
    expect(screen.queryByText("qr_ordering_enabled")).toBeNull();
    expect(screen.getByText(/QR ordering is switched on Channels & payments/)).toBeTruthy();

    fireEvent.click(screen.getByRole("button", { name: "Full service" }));
    fireEvent.click(screen.getByRole("button", { name: "Publish capabilities" }));
    await waitFor(() => expect(publishCapabilities).toHaveBeenCalled());
    const [, , flags] = publishCapabilities.mock.calls[0] as [string, string, object];
    expect(flags).toEqual({ tables_enabled: true, pay_first_enabled: false });
  });
});
