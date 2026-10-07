// A till's default float is said on the Devices screen (ADR-0167 decision 4).
//
// Where a store keeps a drawer for each till, a terminal may name the float its drawer opens with,
// the store's float until somebody says. What these pin:
//
//   * a terminal nobody has set reads as the store's float, and the form says what that is now,
//     from the store's published configuration;
//   * a float is typed as money, in whole units of the store's currency, and saved in minor units
//     with the version the row was read at; an empty field saves the store's, as `null`;
//   * a float beyond the bounds the cloud takes is refused before anything is sent, in the
//     store's currency;
//   * honour or hide (ADR-0160 decision 5): a store whose edge is older than 0.14.1 is offered no
//     Float action, and one line says why, while the column still shows what was saved; a store
//     that has not said which release it runs is offered it with a note;
//   * a store whose currency cannot be read is offered no float, because a figure typed without
//     its decimals could be a hundred times what was meant.

import { cleanup, fireEvent, render, screen, waitFor, within } from "@solidjs/testing-library";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import type { DeviceProposalSummary } from "../src/api/types";
import { Devices } from "../src/screens/Devices";
import { selectStore, selectTenant } from "../src/state/session";

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
const setTerminalFloat = vi.fn();

vi.mock("../src/api/client", () => ({
  api: {
    listProposals: () => Promise.resolve([]),
    listStores: () => Promise.resolve([]),
    listStoreDevices: (...args: unknown[]) => listStoreDevices(...args),
    fleetStore: (...args: unknown[]) => fleetStore(...args),
    effectiveConfig: (...args: unknown[]) => effectiveConfig(...args),
    listStations: () => Promise.resolve([]),
    listAudit: () => Promise.resolve([]),
    setTerminalFloat: (...args: unknown[]) => setTerminalFloat(...args),
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

/** Opens the float form for the one till, once the store's currency has been read. */
async function openFloat(card: HTMLElement): Promise<HTMLElement> {
  const button = within(card).getByRole("button", { name: "Float" }) as HTMLButtonElement;
  await waitFor(() => expect(button.disabled).toBe(false));
  fireEvent.click(button);
  return screen.findByRole("dialog");
}

/** The float's input: the field's currency sits inside its label. */
const floatInput = (panel: HTMLElement) =>
  within(panel).getByLabelText(/^Default float/) as HTMLInputElement;

describe("a till's default float", () => {
  beforeEach(() => {
    localStorage.clear();
    vi.clearAllMocks();
    setTerminalFloat.mockResolvedValue(undefined);
    listStoreDevices.mockResolvedValue([TILL]);
    fleetStore.mockResolvedValue({ installed_version: "0.14.1" });
    effectiveConfig.mockResolvedValue(published("VND", 200_000));
    selectTenant(TENANT.id, TENANT.name);
    selectStore(STORE.id, STORE.name);
  });
  afterEach(cleanup);

  it("reads as the store's float until somebody says, and saves what they type", async () => {
    render(() => <Devices />);
    const card = await terminalsCard();
    expect(within(card).getByText("The store's float")).toBeTruthy();

    const panel = await openFloat(card);
    expect(within(panel).getByText("The store's float is 200,000 VND now.")).toBeTruthy();
    expect(floatInput(panel).value).toBe("");

    fireEvent.input(floatInput(panel), { target: { value: "500000" } });
    fireEvent.click(within(panel).getByRole("button", { name: "Save" }));
    await waitFor(() =>
      expect(setTerminalFloat).toHaveBeenCalledWith(TENANT.id, TILL.id, 500_000, TILL.version),
    );
  });

  it("takes whole units of a currency with decimals, and an empty field is the store's", async () => {
    effectiveConfig.mockResolvedValue(published("INR"));
    listStoreDevices.mockResolvedValue([{ ...TILL, opening_float_minor: 250_000 }]);
    render(() => <Devices />);
    const card = await terminalsCard();
    expect(await within(card).findByText("2,500.00 INR")).toBeTruthy();

    const panel = await openFloat(card);
    // A store whose shift node names no float opens its drawers on 0, which fills in nothing.
    expect(within(panel).getByText("The store's float is 0.00 INR now.")).toBeTruthy();
    expect(floatInput(panel).value).toBe("2,500.00");
    fireEvent.input(floatInput(panel), { target: { value: "1500.50" } });
    fireEvent.click(within(panel).getByRole("button", { name: "Save" }));
    await waitFor(() =>
      expect(setTerminalFloat).toHaveBeenCalledWith(TENANT.id, TILL.id, 150_050, TILL.version),
    );

    const again = await openFloat(card);
    fireEvent.input(floatInput(again), { target: { value: "" } });
    fireEvent.click(within(again).getByRole("button", { name: "Save" }));
    await waitFor(() =>
      expect(setTerminalFloat).toHaveBeenLastCalledWith(TENANT.id, TILL.id, null, TILL.version),
    );
  });

  it("refuses a float beyond the bounds in the store's currency, and sends nothing", async () => {
    render(() => <Devices />);
    const panel = await openFloat(await terminalsCard());
    fireEvent.input(floatInput(panel), { target: { value: "1000000001" } });
    fireEvent.click(within(panel).getByRole("button", { name: "Save" }));
    expect(
      await within(panel).findByText("A float is from 0 VND to 1,000,000,000 VND."),
    ).toBeTruthy();
    expect(setTerminalFloat).not.toHaveBeenCalled();
  });

  it("offers no Float on a store whose release does not read it, and still shows what was saved", async () => {
    fleetStore.mockResolvedValue({ installed_version: "0.14.0" });
    listStoreDevices.mockResolvedValue([{ ...TILL, opening_float_minor: 350_000 }]);
    render(() => <Devices />);
    const card = await terminalsCard();
    expect(
      await within(card).findByText(
        "A till's float cannot be set for Ben Thanh yet: it runs release 0.14.0, which does not " +
          "read one, so every drawer opens on the store's float, whatever is saved here, until it " +
          "updates to 0.14.1.",
      ),
    ).toBeTruthy();
    expect(await within(card).findByText("350,000 VND")).toBeTruthy();
    expect(within(card).queryByRole("button", { name: "Float" })).toBeNull();
  });

  it("offers Float with a note where the store has not said which release it runs", async () => {
    fleetStore.mockRejectedValue(new Error("the fleet read failed"));
    render(() => <Devices />);
    const card = await terminalsCard();
    expect(
      await within(card).findByText(
        "Ben Thanh has not reported which release it runs, so its tills may not open their " +
          "drawers on their own float yet. A till's float is honoured from release 0.14.1.",
      ),
    ).toBeTruthy();
    expect(await openFloat(card)).toBeTruthy();
  });

  it("offers no float where the store's currency cannot be read, and says why", async () => {
    effectiveConfig.mockRejectedValue(new Error("the config read failed"));
    listStoreDevices.mockResolvedValue([{ ...TILL, opening_float_minor: 350_000 }]);
    render(() => <Devices />);
    const card = await terminalsCard();
    expect(
      await within(card).findByText(
        "A till's float can be set once the currency Ben Thanh trades in can be read from its " +
          "published configuration.",
      ),
    ).toBeTruthy();
    expect(within(card).getByText("350,000 minor units")).toBeTruthy();
    const button = within(card).getByRole("button", { name: "Float" }) as HTMLButtonElement;
    expect(button.disabled).toBe(true);
  });
});
