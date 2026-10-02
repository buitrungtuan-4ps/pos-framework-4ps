// Which language a kitchen station's tickets print in is said on the Stations screen (ADR-0160
// decision 2).
//
// Every kitchen ticket printed in the store's display language. Each station now says which of the
// receipt language's four choices its cooks read. What these pin:
//
//   * the form offers exactly the four choices the receipt language setting offers, in its words;
//   * a station nobody has set shows the display language, in the list and in the form, because
//     that is what it prints in, and saving it leaves it unset rather than writing a choice nobody
//     made;
//   * a chosen language is sent as its token with the version the row was read at;
//   * a language from a newer cloud is shown by its token rather than as nothing;
//   * archiving a station sends back the language it has, because an update is the station's whole
//     new state and would otherwise clear it.

import { cleanup, fireEvent, render, screen, waitFor, within } from "@solidjs/testing-library";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import type { Station } from "../src/api/types";
import { Stations } from "../src/screens/Stations";
import { selectStore, selectTenant, setActingAdmin } from "../src/state/session";

const TENANT = { id: "01M22190WCY5PS7KCA7ET7H679", name: "Pizza 4P's Vietnam" };
const STORE = { id: "01STOREAAAAAAAAAAAAAAAAAAA", name: "Ben Thanh" };

const GRILL: Station = {
  station_id: "01STATIONGRILLAAAAAAAAAAAA",
  tenant_id: TENANT.id,
  store_id: STORE.id,
  name: "Grill",
  backup_station_id: null,
  is_default: false,
  late_after_seconds: null,
  ticket_language: null,
  status: "active",
  etag: "3",
};

const listStations = vi.fn();
const updateStation = vi.fn();

vi.mock("../src/api/client", () => ({
  api: {
    listStations: (...args: unknown[]) => listStations(...args),
    listRoutingRules: () => Promise.resolve([]),
    listItems: () => Promise.resolve([]),
    configNodes: () => Promise.resolve({ current_version_id: null, nodes: [] }),
    updateStation: (...args: unknown[]) => updateStation(...args),
    createStation: vi.fn(),
    publishFloor: vi.fn(),
  },
  ApiError: class ApiError extends Error {},
}));

/** Opens the editor for the one station on the page, once the store's stations have loaded. */
async function openEditor(): Promise<HTMLElement> {
  fireEvent.click(await screen.findByRole("button", { name: "Edit" }));
  return screen.findByRole("dialog");
}

/** The editor's language choice: its label carries the hint too, so it is found by role. */
function languageField(panel: HTMLElement): HTMLSelectElement {
  const [field] = within(panel).getAllByRole("combobox").filter((select) =>
    select.closest("label")?.textContent?.startsWith("Ticket language"),
  );
  if (!(field instanceof HTMLSelectElement)) {
    throw new Error("the editor has a ticket language field");
  }
  return field;
}

describe("the language a station's tickets print in", () => {
  beforeEach(() => {
    localStorage.clear();
    vi.clearAllMocks();
    updateStation.mockResolvedValue(undefined);
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

  it("offers the receipt language's four choices, and saves the one chosen", async () => {
    listStations.mockResolvedValue([GRILL]);
    render(() => <Stations />);
    expect(await screen.findByText("The store's display language")).toBeTruthy();

    const panel = await openEditor();
    const field = languageField(panel);
    expect(field.value).toBe("RECEIPT_LANGUAGE_DISPLAY");
    expect(Array.from(field.options).map((option) => option.textContent)).toEqual([
      "The store's display language",
      "The language of the store's country",
      "Vietnamese",
      "English",
    ]);

    fireEvent.change(field, { target: { value: "RECEIPT_LANGUAGE_EN" } });
    fireEvent.click(within(panel).getByRole("button", { name: "Save" }));

    await waitFor(() =>
      expect(updateStation).toHaveBeenCalledWith(
        GRILL.station_id,
        TENANT.id,
        {
          name: "Grill",
          backupStationId: null,
          isDefault: false,
          lateAfterSeconds: null,
          ticketLanguage: "RECEIPT_LANGUAGE_EN",
          status: "active",
        },
        GRILL.etag,
      ),
    );
  });

  it("leaves a station on the display language unset rather than choosing for it", async () => {
    listStations.mockResolvedValue([{ ...GRILL, ticket_language: "RECEIPT_LANGUAGE_VI" }]);
    render(() => <Stations />);
    expect(await screen.findByText("Vietnamese")).toBeTruthy();

    const panel = await openEditor();
    const field = languageField(panel);
    expect(field.value).toBe("RECEIPT_LANGUAGE_VI");
    fireEvent.change(field, { target: { value: "RECEIPT_LANGUAGE_DISPLAY" } });
    fireEvent.click(within(panel).getByRole("button", { name: "Save" }));

    await waitFor(() => expect(updateStation).toHaveBeenCalledTimes(1));
    expect(updateStation.mock.calls[0]?.[2]).toMatchObject({ ticketLanguage: null });
  });

  it("shows a language from a newer cloud by its token, and keeps it when archiving", async () => {
    listStations.mockResolvedValue([{ ...GRILL, ticket_language: "RECEIPT_LANGUAGE_JA" }]);
    render(() => <Stations />);
    expect(await screen.findByText("RECEIPT_LANGUAGE_JA")).toBeTruthy();

    fireEvent.click(await screen.findByRole("button", { name: "Archive" }));
    const dialog = await screen.findByRole("dialog");
    fireEvent.click(within(dialog).getByRole("button", { name: "Archive" }));

    await waitFor(() => expect(updateStation).toHaveBeenCalledTimes(1));
    expect(updateStation.mock.calls[0]?.[2]).toMatchObject({
      ticketLanguage: "RECEIPT_LANGUAGE_JA",
      status: "archived",
    });
  });
});
