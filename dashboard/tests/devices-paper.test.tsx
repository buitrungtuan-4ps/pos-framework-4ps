// A printer's paper and cutter are said on the Devices screen (ADR-0160 decision 2).
//
// The edge lays a receipt out for the paper the console says a printer takes, and sends no cut to a
// printer the console says has no cutter. What these pin:
//
//   * a printer nobody has set reads as the 80 mm printer with a cutter that every store was taken
//     to have, on the card and in the form, because that is what the till prints it as;
//   * the form offers the three papers the cloud accepts and nothing else;
//   * saving sends the wire token and the cutter with the version the row was read at, as the
//     drawer mark does, so a colleague's change is not silently overwritten;
//   * honour or hide (decision 5): only an edge from 0.14.1 reads a printer's paper, so a store on
//     an older release is offered no Paper action, and one line says why, while the column still
//     shows what was saved; a store that has not said which release it runs is offered it with a
//     note.

import { cleanup, fireEvent, render, screen, waitFor, within } from "@solidjs/testing-library";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import type { DeviceProposalSummary } from "../src/api/types";
import { Devices } from "../src/screens/Devices";
import { selectStore, selectTenant } from "../src/state/session";

const TENANT = { id: "01M22190WCY5PS7KCA7ET7H679", name: "Pizza 4P's Vietnam" };
const STORE = { id: "01STOREAAAAAAAAAAAAAAAAAAA", name: "Ben Thanh" };

const PRINTER: DeviceProposalSummary = {
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

const listStoreDevices = vi.fn();
const fleetStore = vi.fn();
const setPrinterPaper = vi.fn();

vi.mock("../src/api/client", () => ({
  api: {
    listProposals: () => Promise.resolve([]),
    listStores: () => Promise.resolve([]),
    listStoreDevices: (...args: unknown[]) => listStoreDevices(...args),
    fleetStore: (...args: unknown[]) => fleetStore(...args),
    listStations: () => Promise.resolve([]),
    listAudit: () => Promise.resolve([]),
    setPrinterPaper: (...args: unknown[]) => setPrinterPaper(...args),
    setDrawerAttached: vi.fn(),
    setPrintAgent: vi.fn(),
    publishDevices: vi.fn(),
  },
  ApiError: class ApiError extends Error {},
}));

/** Opens the paper form for the one printer on the page, once the store's devices have loaded. */
async function openPaper(): Promise<HTMLElement> {
  fireEvent.click(await screen.findByRole("button", { name: "Paper" }));
  return screen.findByRole("dialog");
}

describe("a printer's paper", () => {
  beforeEach(() => {
    localStorage.clear();
    vi.clearAllMocks();
    setPrinterPaper.mockResolvedValue(undefined);
    // The release that first reads a printer's paper, unless a test says otherwise.
    fleetStore.mockResolvedValue({ installed_version: "0.14.1" });
    selectTenant(TENANT.id, TENANT.name);
    selectStore(STORE.id, STORE.name);
  });
  afterEach(cleanup);

  it("reads as 80 mm with a cutter until somebody says, and saves what they say", async () => {
    listStoreDevices.mockResolvedValue([PRINTER]);
    render(() => <Devices />);
    expect(await screen.findByText("80 mm")).toBeTruthy();

    const panel = await openPaper();
    const paper = within(panel).getByLabelText("Paper") as HTMLSelectElement;
    expect(paper.value).toBe("PAPER_WIDTH_MILLIMETRES_80");
    expect(Array.from(paper.options).map((option) => option.textContent)).toEqual([
      "80 mm",
      "80 mm, 48 characters a line",
      "58 mm",
    ]);
    // The dialog's one checkbox: its label carries the hint too, so it is found by role.
    const cuts = within(panel).getByRole("checkbox") as HTMLInputElement;
    expect(cuts.closest("label")?.textContent).toContain("Cuts paper");
    expect(cuts.checked).toBe(true);

    fireEvent.change(paper, { target: { value: "PAPER_WIDTH_MILLIMETRES_58" } });
    fireEvent.click(cuts);
    fireEvent.click(within(panel).getByRole("button", { name: "Save" }));

    await waitFor(() =>
      expect(setPrinterPaper).toHaveBeenCalledWith(
        TENANT.id,
        PRINTER.id,
        "PAPER_WIDTH_MILLIMETRES_58",
        false,
        PRINTER.version,
      ),
    );
  });

  it("shows a printer with no cutter as such, and opens its form as it was set", async () => {
    listStoreDevices.mockResolvedValue([
      { ...PRINTER, paper_width: "PAPER_WIDTH_MILLIMETRES_58", cuts_paper: false },
    ]);
    render(() => <Devices />);
    expect(await screen.findByText("58 mm, no cutter")).toBeTruthy();

    const panel = await openPaper();
    expect((within(panel).getByLabelText("Paper") as HTMLSelectElement).value).toBe(
      "PAPER_WIDTH_MILLIMETRES_58",
    );
    expect((within(panel).getByRole("checkbox") as HTMLInputElement).checked).toBe(false);
  });

  it("offers no Paper on a store whose release does not read it, and says why", async () => {
    fleetStore.mockResolvedValue({ installed_version: "0.14.0" });
    listStoreDevices.mockResolvedValue([
      PRINTER,
      { ...PRINTER, id: "01PRINTERBBBBBBBBBBBBBBBBB", name: "Bar", address: "192.0.2.11:9100" },
    ]);
    render(() => <Devices />);
    expect(
      await screen.findByText(
        "Paper cannot be set for Ben Thanh yet: it runs release 0.14.0, which does not read a " +
          "printer's paper, so it prints on 80 mm paper with a cutter, whatever is saved here, " +
          "until it updates to 0.14.1.",
      ),
    ).toBeTruthy();
    expect(screen.getAllByRole("button", { name: "Choose agent" })).toHaveLength(2);
    expect(screen.queryAllByRole("button", { name: "Paper" })).toHaveLength(0);
    expect(screen.queryByRole("dialog")).toBeNull();
  });

  it("still shows the paper a store on an older release has saved", async () => {
    fleetStore.mockResolvedValue({ installed_version: "0.14.0" });
    listStoreDevices.mockResolvedValue([
      { ...PRINTER, paper_width: "PAPER_WIDTH_MILLIMETRES_58", cuts_paper: false },
    ]);
    render(() => <Devices />);
    // Once the release is read, so that a missing action is the gate and not the read still out.
    expect(await screen.findByText(/^Paper cannot be set for Ben Thanh yet/)).toBeTruthy();
    expect(screen.getByText("58 mm, no cutter")).toBeTruthy();
    expect(screen.queryByRole("button", { name: "Paper" })).toBeNull();
  });

  it("offers Paper with a note where the store has not said which release it runs", async () => {
    fleetStore.mockRejectedValue(new Error("the fleet read failed"));
    listStoreDevices.mockResolvedValue([PRINTER]);
    render(() => <Devices />);
    expect(
      await screen.findByText(
        "Ben Thanh has not reported which release it runs, so it may not honour a printer's " +
          "paper yet. Paper is honoured from release 0.14.1.",
      ),
    ).toBeTruthy();
    expect(await openPaper()).toBeTruthy();
  });

  it("offers Paper with no line at all to a store on a later release", async () => {
    fleetStore.mockResolvedValue({ installed_version: "0.15.2" });
    listStoreDevices.mockResolvedValue([PRINTER]);
    render(() => <Devices />);
    expect(await screen.findByRole("button", { name: "Paper" })).toBeTruthy();
    expect(screen.queryByText(/printer's paper/)).toBeNull();
  });
});
