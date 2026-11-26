// After a staged publish (ADR-0246 decision 3) the card starts again and the history shows the
// change: each staged select reads "Unchanged" once the stage is cleared, and the Settings history
// list below reloads, so the publish just made is listed without reloading the page.

import { cleanup, fireEvent, render, screen, waitFor } from "@solidjs/testing-library";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { SettingsHistory } from "../src/screens/SettingsHistory";
import { selectTenant } from "../src/state/session";

const TENANT = { id: "01M22190WCY5PS7KCA7ET7H679", name: "Fake Pizza Tenant" };
const STORE = { store_id: "01STRAAAAAAAAAAAAAAAAAAAAA", name: "Fake Store One" };

const listAudit = vi.fn();
const putSetting = vi.fn();

vi.mock("../src/api/client", () => ({
  api: {
    listAudit: (...args: unknown[]) => listAudit(...args),
    listStores: () => Promise.resolve([STORE]),
    listBrands: () => Promise.resolve([]),
    listStoreGroups: () => Promise.resolve([]),
    settingsCatalogue: () =>
      Promise.resolve([
        {
          setting_key: "einvoice.correction_kind",
          node: "einvoice",
          field: "correction_kind",
          kind: "SETTING_KIND_CHOICE",
          scopes: ["SETTING_SCOPE_TENANT", "SETTING_SCOPE_STORE"],
          values: ["EINVOICE_CORRECTION_KIND_ADJUSTMENT", "EINVOICE_CORRECTION_KIND_REPLACEMENT"],
          since: "0.14.1",
          default: "EINVOICE_CORRECTION_KIND_ADJUSTMENT",
        },
      ]),
    putSetting: (...args: unknown[]) => putSetting(...args),
  },
  ApiError: class ApiError extends Error {},
}));

/** The select labelled `name`. */
function select(name: string): HTMLSelectElement {
  return screen.getByRole("combobox", { name }) as HTMLSelectElement;
}

describe("the staged publish card after Publish", () => {
  beforeEach(() => {
    localStorage.clear();
    vi.clearAllMocks();
    listAudit.mockResolvedValue([]);
    putSetting.mockResolvedValue({});
    selectTenant(TENANT.id, TENANT.name);
  });
  afterEach(cleanup);

  it("shows the staged setting as unchanged again and reloads the history", async () => {
    render(() => <SettingsHistory />);
    await waitFor(() => expect(screen.getAllByRole("option", { name: "einvoice" }).length).toBeGreaterThan(0));
    fireEvent.change(select("Group"), { target: { value: "einvoice" } });
    const staged = await screen.findByRole("combobox", { name: "Correction of a voided invoiced bill" });
    fireEvent.change(staged, { target: { value: "EINVOICE_CORRECTION_KIND_REPLACEMENT" } });
    expect((staged as HTMLSelectElement).value).toBe("EINVOICE_CORRECTION_KIND_REPLACEMENT");
    fireEvent.change(select("Publish to"), { target: { value: `SETTING_SCOPE_STORE:${STORE.store_id}` } });
    const readsBefore = listAudit.mock.calls.length;

    fireEvent.click(screen.getByRole("button", { name: "Publish 1 staged change(s)" }));

    await waitFor(() => expect(putSetting).toHaveBeenCalledTimes(1));
    expect(putSetting.mock.calls[0]).toEqual([
      TENANT.id,
      "einvoice.correction_kind",
      "SETTING_SCOPE_STORE",
      STORE.store_id,
      "EINVOICE_CORRECTION_KIND_REPLACEMENT",
    ]);
    await waitFor(() => expect(listAudit.mock.calls.length).toBe(readsBefore + 1));
    const after = select("Correction of a voided invoiced bill");
    expect(after.value).toBe("");
    expect(after.selectedOptions[0]?.textContent).toBe("Unchanged");
    expect(screen.getByRole("button", { name: "Publish 0 staged change(s)" })).toHaveProperty("disabled", true);
  });
});
