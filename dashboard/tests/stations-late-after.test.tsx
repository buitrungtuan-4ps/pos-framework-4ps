// When a kitchen station's tickets are late is said on the Stations screen (ADR-0160 decision 2).
//
// Every kitchen display marked a ticket late after ten minutes. Each station now says its own, and
// the store reads a station that says nothing as those ten minutes. What these pin:
//
//   * a station nobody has set shows the ten minutes it runs, in the list and as an empty field;
//   * the form takes whole minutes from 1 to 60 and sends seconds, with the version the row was read
//     at, so a colleague's change is not silently overwritten;
//   * a value outside the minutes is refused in the form and never sent;
//   * archiving a station sends back the threshold it has, because an update is the station's whole
//     new state and would otherwise clear it.

import { cleanup, fireEvent, render, screen, waitFor, within } from "@solidjs/testing-library";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import type { Station } from "../src/api/types";
import { Stations } from "../src/screens/Stations";
import { selectStore, selectTenant, setActingAdmin } from "../src/state/session";

const TENANT = { id: "01M22190WCY5PS7KCA7ET7H679", name: "Pizza 4P's Vietnam" };
const STORE = { id: "01STOREAAAAAAAAAAAAAAAAAAA", name: "Ben Thanh" };

const OVEN: Station = {
  station_id: "01STATIONOVENAAAAAAAAAAAAA",
  tenant_id: TENANT.id,
  store_id: STORE.id,
  name: "Oven",
  backup_station_id: null,
  is_default: true,
  late_after_seconds: null,
  ticket_language: null,
  status: "active",
  etag: "7",
};

const listStations = vi.fn();
const updateStation = vi.fn();
const createStation = vi.fn();

vi.mock("../src/api/client", () => ({
  api: {
    listStations: (...args: unknown[]) => listStations(...args),
    listRoutingRules: () => Promise.resolve([]),
    listItems: () => Promise.resolve([]),
    configNodes: () => Promise.resolve({ current_version_id: null, nodes: [] }),
    updateStation: (...args: unknown[]) => updateStation(...args),
    createStation: (...args: unknown[]) => createStation(...args),
    publishFloor: vi.fn(),
  },
  ApiError: class ApiError extends Error {},
}));

/** Opens the editor for the one station on the page, once the store's stations have loaded. */
async function openEditor(): Promise<HTMLElement> {
  fireEvent.click(await screen.findByRole("button", { name: "Edit" }));
  return screen.findByRole("dialog");
}

describe("when a station's tickets are late", () => {
  beforeEach(() => {
    localStorage.clear();
    vi.clearAllMocks();
    updateStation.mockResolvedValue(undefined);
    createStation.mockResolvedValue({ id: "01STATIONNEWAAAAAAAAAAAAAA" });
    setActingAdmin({
      id: "01ADMINAAAAAAAAAAAAAAAAAAA",
      email: "admin@example.test",
      name: "Admin",
      role: "owner",
      status: "active",
    });
    selectTenant(TENANT.id, TENANT.name);
    selectStore(STORE.id, STORE.name);
  });
  afterEach(() => {
    cleanup();
    setActingAdmin(null);
  });

  it("reads as ten minutes until somebody says, and saves the minutes as seconds", async () => {
    listStations.mockResolvedValue([OVEN]);
    render(() => <Stations />);
    expect(await screen.findByText("10 minutes (the default)")).toBeTruthy();

    const panel = await openEditor();
    // The drawer's one number field: its label carries the hint too, so it is found by role.
    const field = within(panel).getByRole("spinbutton") as HTMLInputElement;
    expect(field.closest("label")?.textContent).toContain("Late after (minutes)");
    expect(field.value).toBe("");

    fireEvent.input(field, { target: { value: "4" } });
    fireEvent.click(within(panel).getByRole("button", { name: "Save" }));

    await waitFor(() =>
      expect(updateStation).toHaveBeenCalledWith(
        OVEN.station_id,
        TENANT.id,
        {
          name: "Oven",
          backupStationId: null,
          isDefault: true,
          lateAfterSeconds: 240,
          ticketLanguage: null,
          status: "active",
        },
        OVEN.etag,
      ),
    );
  });

  it("shows a station's own minutes, and empties the field to give it back the default", async () => {
    listStations.mockResolvedValue([{ ...OVEN, late_after_seconds: 300 }]);
    render(() => <Stations />);
    expect(await screen.findByText("5 minutes")).toBeTruthy();

    const panel = await openEditor();
    const field = within(panel).getByRole("spinbutton") as HTMLInputElement;
    expect(field.value).toBe("5");
    fireEvent.input(field, { target: { value: "" } });
    fireEvent.click(within(panel).getByRole("button", { name: "Save" }));

    await waitFor(() => expect(updateStation).toHaveBeenCalledTimes(1));
    expect(updateStation.mock.calls[0]?.[2]).toMatchObject({ lateAfterSeconds: null });
  });

  it("refuses minutes it cannot send, and sends nothing", async () => {
    listStations.mockResolvedValue([OVEN]);
    render(() => <Stations />);
    const panel = await openEditor();
    const field = within(panel).getByRole("spinbutton") as HTMLInputElement;
    for (const typed of ["0", "61", "2.5"]) {
      fireEvent.input(field, { target: { value: typed } });
      fireEvent.click(within(panel).getByRole("button", { name: "Save" }));
      expect(
        await screen.findByText(
          "Late after must be a whole number of minutes from 1 to 60, or empty for 10 minutes.",
        ),
      ).toBeTruthy();
    }
    expect(updateStation).not.toHaveBeenCalled();
  });

  it("sends a new station's minutes as seconds", async () => {
    listStations.mockResolvedValue([]);
    render(() => <Stations />);
    fireEvent.click(await screen.findByRole("button", { name: "Add a station" }));
    const panel = await screen.findByRole("dialog");
    fireEvent.input(within(panel).getByLabelText("Station name"), { target: { value: "Grill" } });
    fireEvent.input(within(panel).getByRole("spinbutton"), {
      target: { value: "15" },
    });
    fireEvent.click(within(panel).getByRole("button", { name: "Save" }));

    await waitFor(() =>
      expect(createStation).toHaveBeenCalledWith(TENANT.id, STORE.id, {
        name: "Grill",
        backupStationId: null,
        isDefault: false,
        lateAfterSeconds: 900,
        ticketLanguage: null,
      }),
    );
  });

  it("keeps a station's threshold when it is archived", async () => {
    listStations.mockResolvedValue([{ ...OVEN, late_after_seconds: 300 }]);
    render(() => <Stations />);
    fireEvent.click(await screen.findByRole("button", { name: "Archive" }));
    const dialog = await screen.findByRole("dialog");
    fireEvent.click(within(dialog).getByRole("button", { name: "Archive" }));

    await waitFor(() => expect(updateStation).toHaveBeenCalledTimes(1));
    expect(updateStation.mock.calls[0]?.[2]).toMatchObject({
      lateAfterSeconds: 300,
      status: "archived",
    });
  });
});
