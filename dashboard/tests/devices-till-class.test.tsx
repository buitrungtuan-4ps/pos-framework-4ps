// A till's device class is said on the Devices screen (ADR-0201 decision 3).
//
// The till bound to a terminal draws each package screen in the layout for its class, and reads
// the class from its screen until somebody says. What these pin:
//
//   * a terminal nobody has set reads "Read from its screen";
//   * a class is chosen from the release's classes and saved as its token with the version the
//     row was read at; the empty choice saves `null`, the screen's again;
//   * honour or hide: a store whose edge is older than 0.14.1 is offered no action, while the
//     column still shows what was saved.

import { cleanup, fireEvent, render, screen, waitFor, within } from "@solidjs/testing-library";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { Devices } from "../src/screens/Devices";
import { selectStore, selectTenant } from "../src/state/session";
import type { DeviceProposalSummary } from "../src/api/schemas";

const TENANT = { id: "01M22190WCY5PS7KCA7ET7H679", name: "Pizza 4P's Vietnam" };
const STORE = { id: "01STOREAAAAAAAAAAAAAAAAAAA", name: "Ben Thanh" };

const TILL: DeviceProposalSummary = {
  id: "01TERMINALAAAAAAAAAAAAAAAA",
  store_id: STORE.id,
  kind: "terminal",
  name: "Bar till",
  address: "",
  connection: null,
  station_id: null,
  agent_device_id: null,
  drawer_attached: false,
  paper_width: null,
  cuts_paper: null,
  receipt_printer_id: null,
  receipt_language: null,
  receipt_second_language: null,
  opening_float_minor: null,
  device_class: null,
  status: "approved",
  version: "9",
};

/** A store's published configuration, as much of it as the float reads. */
const published = (currency: string, storeFloat?: number) => ({
  locale: { currency_code: currency },
  shift: storeFloat === undefined ? {} : { opening_float_minor: storeFloat },
});

const listStoreDevices = vi.fn();
const fleetStore = vi.fn();
const effectiveConfig = vi.fn();
const setTerminalDeviceClass = vi.fn();

vi.mock("../src/api/client", () => ({
  api: {
    listProposals: () => Promise.resolve([]),
    listStores: () => Promise.resolve([]),
    listStoreDevices: (...args: unknown[]) => listStoreDevices(...args),
    fleetStore: (...args: unknown[]) => fleetStore(...args),
    effectiveConfig: (...args: unknown[]) => effectiveConfig(...args),
    listStations: () => Promise.resolve([]),
    listAudit: () => Promise.resolve([]),
    setTerminalFloat: vi.fn(),
    setTerminalDeviceClass: (...args: unknown[]) => setTerminalDeviceClass(...args),
    setTerminalReceipt: vi.fn(),
    setPrinterPaper: vi.fn(),
    setDrawerAttached: vi.fn(),
    setPrintAgent: vi.fn(),
    publishDevices: vi.fn(),
  },
  ApiError: class ApiError extends Error {},
}));

/** The terminals card, once the store's devices have loaded. */
async function terminalsCard(): Promise<HTMLElement> {
  await screen.findByText("Bar till");
  const heading = screen.getByRole("heading", { name: "Terminals" });
  const card = heading.closest("section") ?? heading.parentElement?.parentElement;
  if (!card) {
    throw new Error("the terminals card");
  }
  return card;
}


/** Opens the device class form for the one till. */
async function openClass(card: HTMLElement): Promise<HTMLElement> {
  fireEvent.click(within(card).getByRole("button", { name: "Device class" }));
  return screen.findByRole("dialog");
}

const classSelect = (panel: HTMLElement) => within(panel).getByLabelText("Device class") as HTMLSelectElement;

describe("a till's device class", () => {
  beforeEach(() => {
    localStorage.clear();
    vi.clearAllMocks();
    setTerminalDeviceClass.mockResolvedValue(undefined);
    listStoreDevices.mockResolvedValue([TILL]);
    fleetStore.mockResolvedValue({ installed_version: "0.14.1" });
    effectiveConfig.mockResolvedValue(published("VND", 200_000));
    selectTenant(TENANT.id, TENANT.name);
    selectStore(STORE.id, STORE.name);
  });
  afterEach(cleanup);

  it("reads its screen until somebody says, and saves the class chosen", async () => {
    render(() => <Devices />);
    const card = await terminalsCard();
    expect(within(card).getByText("Read from its screen")).toBeTruthy();

    const panel = await openClass(card);
    expect(classSelect(panel).value).toBe("");
    fireEvent.change(classSelect(panel), { target: { value: "DEVICE_CLASS_TABLET" } });
    fireEvent.click(within(panel).getByRole("button", { name: "Save" }));
    await waitFor(() =>
      expect(setTerminalDeviceClass).toHaveBeenCalledWith(TENANT.id, TILL.id, "DEVICE_CLASS_TABLET", TILL.version),
    );
  });

  it("shows the class saved, and the empty choice is the screen's again", async () => {
    listStoreDevices.mockResolvedValue([{ ...TILL, device_class: "DEVICE_CLASS_KITCHEN_DISPLAY" }]);
    render(() => <Devices />);
    const card = await terminalsCard();
    expect(await within(card).findByText("Kitchen display")).toBeTruthy();

    const panel = await openClass(card);
    expect(classSelect(panel).value).toBe("DEVICE_CLASS_KITCHEN_DISPLAY");
    fireEvent.change(classSelect(panel), { target: { value: "" } });
    fireEvent.click(within(panel).getByRole("button", { name: "Save" }));
    await waitFor(() => expect(setTerminalDeviceClass).toHaveBeenCalledWith(TENANT.id, TILL.id, null, TILL.version));
  });

  it("offers no action on a store whose release does not read it, and still shows what was saved", async () => {
    fleetStore.mockResolvedValue({ installed_version: "0.14.0" });
    listStoreDevices.mockResolvedValue([{ ...TILL, device_class: "DEVICE_CLASS_PHONE" }]);
    render(() => <Devices />);
    const card = await terminalsCard();
    expect(await within(card).findByText("Phone")).toBeTruthy();
    expect(within(card).queryByRole("button", { name: "Device class" })).toBeNull();
  });
});
