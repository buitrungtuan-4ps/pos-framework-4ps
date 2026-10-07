// A till's receipts are said on the Devices screen (ADR-0160 decision 4).
//
// A terminal names the printer its receipts, receipt copies and pre-bills go to, and the languages
// they print in, each the store's until somebody says. What these pin:
//
//   * a terminal nobody has set reads as the store's, on the card and in the form;
//   * the printer picker offers the store's printers that serve no station, and nothing else, and
//     each language picker the setting's own choices beside the store's;
//   * saving sends what was chosen, the store's as `null`, with the version the row was read at;
//   * honour or hide (decision 5): a store whose edge is older than 0.14.1 is offered none of it,
//     and one line says why, while a store that has not said which release it runs is offered it
//     with a note.

import { cleanup, fireEvent, render, screen, waitFor, within } from "@solidjs/testing-library";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import type { DeviceProposalSummary } from "../src/api/types";
import { Devices } from "../src/screens/Devices";
import { selectStore, selectTenant } from "../src/state/session";

const TENANT = { id: "01M22190WCY5PS7KCA7ET7H679", name: "Pizza 4P's Vietnam" };
const STORE = { id: "01STOREAAAAAAAAAAAAAAAAAAA", name: "Ben Thanh" };

const DEVICE: DeviceProposalSummary = {
  id: "01PRINTERAAAAAAAAAAAAAAAAA",
  store_id: STORE.id,
  kind: "printer",
  name: "Counter",
  address: "192.0.2.10:9100",
  connection: "network",
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
  version: "7",
};
const BAR_PRINTER = { ...DEVICE, id: "01PRINTERBBBBBBBBBBBBBBBBB", name: "Bar printer" };
const OVEN = {
  ...DEVICE,
  id: "01PRINTERCCCCCCCCCCCCCCCCC",
  name: "Oven",
  station_id: "01STATIONAAAAAAAAAAAAAAAAA",
};
const TILL: DeviceProposalSummary = {
  ...DEVICE,
  id: "01TERMINALAAAAAAAAAAAAAAAA",
  kind: "terminal",
  name: "Bar till",
  address: "",
  connection: null,
  version: "9",
};

const listStoreDevices = vi.fn();
const fleetStore = vi.fn();
const setTerminalReceipt = vi.fn();

vi.mock("../src/api/client", () => ({
  api: {
    listProposals: () => Promise.resolve([]),
    listStores: () => Promise.resolve([]),
    listStoreDevices: (...args: unknown[]) => listStoreDevices(...args),
    fleetStore: (...args: unknown[]) => fleetStore(...args),
    listStations: () => Promise.resolve([]),
    listAudit: () => Promise.resolve([]),
    setTerminalReceipt: (...args: unknown[]) => setTerminalReceipt(...args),
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

/** The labels a select offers, in order. */
const offered = (select: HTMLSelectElement) =>
  Array.from(select.options).map((option) => option.textContent);

describe("a till's receipts", () => {
  beforeEach(() => {
    localStorage.clear();
    vi.clearAllMocks();
    setTerminalReceipt.mockResolvedValue(undefined);
    listStoreDevices.mockResolvedValue([DEVICE, BAR_PRINTER, OVEN, TILL]);
    selectTenant(TENANT.id, TENANT.name);
    selectStore(STORE.id, STORE.name);
  });
  afterEach(cleanup);

  it("reads as the store's until somebody says, and saves what they say", async () => {
    fleetStore.mockResolvedValue({ installed_version: "0.14.1" });
    render(() => <Devices />);
    const card = await terminalsCard();
    expect(await within(card).findByText("The store's receipt printer")).toBeTruthy();
    expect(within(card).getAllByText("The store's")).toHaveLength(2);

    fireEvent.click(within(card).getByRole("button", { name: "Receipts" }));
    const panel = await screen.findByRole("dialog");
    const printer = within(panel).getByLabelText("Receipt printer") as HTMLSelectElement;
    const language = within(panel).getByLabelText("Receipt language") as HTMLSelectElement;
    const second = within(panel).getByLabelText("Second language", {
      exact: false,
    }) as HTMLSelectElement;
    expect([printer.value, language.value, second.value]).toEqual(["", "", ""]);
    expect(offered(printer)).toEqual(["The store's receipt printer", "Counter", "Bar printer"]);
    expect(offered(language)).toEqual([
      "The store's",
      "The store's display language",
      "The language of the store's country",
      "Vietnamese",
      "English",
    ]);
    expect(offered(second)).toEqual(["The store's", "None", "Vietnamese", "English"]);

    fireEvent.change(printer, { target: { value: BAR_PRINTER.id } });
    fireEvent.change(language, { target: { value: "RECEIPT_LANGUAGE_EN" } });
    fireEvent.click(within(panel).getByRole("button", { name: "Save" }));

    await waitFor(() =>
      expect(setTerminalReceipt).toHaveBeenCalledWith(
        TENANT.id,
        TILL.id,
        { printerId: BAR_PRINTER.id, language: "RECEIPT_LANGUAGE_EN", secondLanguage: null },
        TILL.version,
      ),
    );
  });

  it("shows a till as it was set, and opens its form so", async () => {
    fleetStore.mockResolvedValue({ installed_version: "0.15.0" });
    listStoreDevices.mockResolvedValue([
      DEVICE,
      BAR_PRINTER,
      {
        ...TILL,
        receipt_printer_id: BAR_PRINTER.id,
        receipt_language: "RECEIPT_LANGUAGE_EN",
        receipt_second_language: "RECEIPT_SECOND_LANGUAGE_NONE",
      },
    ]);
    render(() => <Devices />);
    const card = await terminalsCard();
    expect(await within(card).findByText("Bar printer")).toBeTruthy();
    expect(within(card).getByText("English")).toBeTruthy();
    expect(within(card).getByText("None")).toBeTruthy();

    fireEvent.click(within(card).getByRole("button", { name: "Receipts" }));
    const panel = await screen.findByRole("dialog");
    expect((within(panel).getByLabelText("Receipt printer") as HTMLSelectElement).value).toBe(
      BAR_PRINTER.id,
    );
    expect((within(panel).getByLabelText("Receipt language") as HTMLSelectElement).value).toBe(
      "RECEIPT_LANGUAGE_EN",
    );
  });

  it("hides them from a store whose release ignores them, and says why", async () => {
    fleetStore.mockResolvedValue({ installed_version: "0.14.0" });
    render(() => <Devices />);
    const card = await terminalsCard();
    expect(
      await within(card).findByText(
        "Each till's receipt printer and languages are hidden for Ben Thanh: it runs release " +
          "0.14.0, and they are honoured from 0.14.1.",
      ),
    ).toBeTruthy();
    expect(within(card).queryByRole("button", { name: "Receipts" })).toBeNull();
    expect(within(card).queryByText("Receipt printer")).toBeNull();
  });

  it("offers them with a note where the store has not said which release it runs", async () => {
    fleetStore.mockRejectedValue(new Error("the fleet read failed"));
    render(() => <Devices />);
    const card = await terminalsCard();
    expect(
      await within(card).findByText(
        "Ben Thanh has not reported which release it runs, so its tills may not print by their " +
          "own receipt printer and languages yet. They are honoured from release 0.14.1.",
      ),
    ).toBeTruthy();
    expect(within(card).getByRole("button", { name: "Receipts" })).toBeTruthy();
  });
});
