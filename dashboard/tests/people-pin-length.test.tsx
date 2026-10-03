// The fewest digits a PIN may have, on the People screen's PIN modal
// ([ADR-0160](../../docs/adr/0160-everything-a-store-runs-differently-is-published-configuration.md)).
//
// The tenant writes `session.pin_min_length` on the Settings screen and the cloud holds every PIN set
// from then on to it. The modal reads it as it opens, so that:
//
//   * its hint names the tenant's range, "6–8 digits" at a minimum of 6, not the 4–8 it said before;
//   * a PIN shorter than the minimum is refused here, before it is sent, in the same words;
//   * a read that fails leaves the modal at 4–8, the rule before the setting. The cloud still refuses
//     a short PIN, so the console guessing low costs a refusal, never a short PIN.

import { createMemoryHistory, MemoryRouter, Route } from "@solidjs/router";
import { cleanup, fireEvent, render, screen, waitFor, within } from "@solidjs/testing-library";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { People } from "../src/screens/People";
import { selectStore, selectTenant, setActingAdmin } from "../src/state/session";
import en from "../src/i18n/en.json";

const messages = en as Record<string, string>;

const TENANT = { id: "01M22190WCY5PS7KCA7ET7H679", name: "Pizza 4P's Vietnam" };
const STORE = { id: "01STOREAAAAAAAAAAAAAAAAAAA", name: "Bến Thành" };

const BAO = {
  employee_id: "01EMPBBBBBBBBBBBBBBBBBBBBB",
  tenant_id: TENANT.id,
  code: "C02",
  name: "Bao",
  status: "active",
  has_pin: false,
  etag: "e1",
};

/** The tenant's minimum, as `GET /admin/settings` lists it. */
const SIX = {
  setting_key: "session.pin_min_length",
  scope: "SETTING_SCOPE_TENANT",
  scope_id: TENANT.id,
  value: 6,
  update_time: "2026-10-03T08:00:00Z",
};

const listSettingValues = vi.fn();
const setEmployeePin = vi.fn();

vi.mock("../src/api/client", () => ({
  api: {
    listRoles: () => Promise.resolve([]),
    permissionCatalogue: () => Promise.resolve([]),
    listStoreGroups: () => Promise.resolve([]),
    listEmployeesPage: () => Promise.resolve({ items: [BAO], total: 1, limit: 12, offset: 0 }),
    listAssignmentsByStore: () => Promise.resolve([]),
    listSettingValues: (...args: unknown[]) => listSettingValues(...args),
    setEmployeePin: (...args: unknown[]) => setEmployeePin(...args),
    publishPermissions: vi.fn(),
  },
  ApiError: class ApiError extends Error {},
}));

/** Mounts the screen, waits for it to load, and opens Bao's PIN modal. */
async function openBaosPin() {
  const history = createMemoryHistory();
  history.set({ value: "/t/tenant/people" });
  render(() => (
    <MemoryRouter history={history}>
      <Route path="/t/:tenant/people" component={People} />
    </MemoryRouter>
  ));
  const open = await screen.findByRole("button", { name: messages["people.setPin"]! });
  // Disabled while the screen loads.
  await waitFor(() => expect((open as HTMLButtonElement).disabled).toBe(false));
  fireEvent.click(open);
  return within(screen.getByRole("dialog"));
}

/** Types `pin` into the modal and presses its Set PIN. */
function send(modal: ReturnType<typeof within>, pin: string) {
  fireEvent.input(modal.getByLabelText(messages["people.pin"]!), { target: { value: pin } });
  fireEvent.click(modal.getByRole("button", { name: messages["people.setPin"]! }));
}

/** The hint the modal shows for a range. */
const hint = (min: number) =>
  messages["people.pinHint"]!.replace("{min}", String(min)).replace("{max}", "8");

describe("the PIN modal holds a PIN to the tenant's minimum", () => {
  beforeEach(() => {
    localStorage.clear();
    vi.clearAllMocks();
    setEmployeePin.mockResolvedValue(undefined);
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

  it("names 6–8 digits at a minimum of 6, and refuses five before sending them", async () => {
    listSettingValues.mockResolvedValue([SIX]);
    const modal = await openBaosPin();

    expect(await modal.findByText(hint(6))).toBeTruthy();
    expect(listSettingValues).toHaveBeenCalledWith(TENANT.id);

    send(modal, "12345");
    expect(await screen.findByText("The PIN must be 6 to 8 digits.")).toBeTruthy();
    expect(setEmployeePin).not.toHaveBeenCalled();

    send(modal, "123456789");
    expect(setEmployeePin).not.toHaveBeenCalled();

    send(modal, "123456");
    await waitFor(() =>
      expect(setEmployeePin).toHaveBeenCalledWith(BAO.employee_id, TENANT.id, "123456"),
    );
  });

  it("falls back to 4–8 when the minimum cannot be read, and leaves the rest to the cloud", async () => {
    listSettingValues.mockRejectedValue(new Error("the settings service is unavailable"));
    const modal = await openBaosPin();

    await waitFor(() => expect(listSettingValues).toHaveBeenCalled());
    expect(modal.getByText(hint(4))).toBeTruthy();

    send(modal, "1234");
    await waitFor(() =>
      expect(setEmployeePin).toHaveBeenCalledWith(BAO.employee_id, TENANT.id, "1234"),
    );
  });
});
