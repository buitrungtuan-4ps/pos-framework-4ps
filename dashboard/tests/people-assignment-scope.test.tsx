// Where an assignment reaches (ADR-0158 decision 3), on the People screen.
//
// A person can be given a role at the store in the top bar, at one of the tenant's store groups, or
// at every store. Three properties are pinned here, because each is a way the screen could look
// right and send something else:
//
//   * the form sends exactly the field the chosen "Where" takes — a store's id, a group's id, or the
//     tenant-wide kind — and never the top-bar store beside a group;
//   * every row of the store's list says where its assignment reaches: this store, the group by
//     name, or every store, so a wider grant is not mistaken for this shop's own;
//   * removing a wider assignment says it leaves every store it reached.

import { cleanup, fireEvent, render, screen, waitFor, within } from "@solidjs/testing-library";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { People } from "../src/screens/People";
import { selectStore, selectTenant, setActingAdmin } from "../src/state/session";
import en from "../src/i18n/en.json";

const messages = en as Record<string, string>;

const TENANT = { id: "01M22190WCY5PS7KCA7ET7H679", name: "Pizza 4P's Vietnam" };
const STORE = { id: "01STOREAAAAAAAAAAAAAAAAAAA", name: "Bến Thành" };

const GROUP = {
  group_id: "01GROUPAAAAAAAAAAAAAAAAAAA",
  tenant_id: TENANT.id,
  name: "Airport branches",
  status: "active",
  store_ids: [STORE.id],
  etag: "g1",
};

const CASHIER = {
  role_template_id: "01ROLEAAAAAAAAAAAAAAAAAAAA",
  tenant_id: TENANT.id,
  name: "Cashier",
  permissions: [],
  permissions_with_approval: [],
  discount_ceiling_minor: null,
  status: "active",
  etag: "r1",
};

const BAO = {
  employee_id: "01EMPBBBBBBBBBBBBBBBBBBBBB",
  tenant_id: TENANT.id,
  code: "C02",
  name: "Bao",
  status: "active",
  has_pin: true,
  etag: "e1",
};

/** One row of each scope at the top-bar store, as `GET /admin/assignments?store_id=` lists them. */
const ROWS = [
  { kind: "ASSIGNMENT_SCOPE_STORE", extra: { store_id: STORE.id }, name: "Alice", code: "C01" },
  {
    kind: "ASSIGNMENT_SCOPE_STORE_GROUP",
    extra: { store_group_id: GROUP.group_id },
    name: "Bao",
    code: "C02",
  },
  { kind: "ASSIGNMENT_SCOPE_TENANT", extra: {}, name: "Cam", code: "C03" },
].map((row, index) => ({
  assignment_id: `01ASSIGN${String(index).padStart(18, "0")}`,
  tenant_id: TENANT.id,
  employee_id: `01EMP${String(index).padStart(21, "0")}`,
  scope_kind: row.kind,
  ...row.extra,
  role_template_id: CASHIER.role_template_id,
  employee_name: row.name,
  employee_code: row.code,
}));

const createAssignment = vi.fn();
const removeAssignment = vi.fn();

vi.mock("../src/api/client", () => ({
  api: {
    listRoles: () => Promise.resolve([CASHIER]),
    permissionCatalogue: () => Promise.resolve([]),
    listStoreGroups: () => Promise.resolve([GROUP]),
    listEmployeesPage: () => Promise.resolve({ items: [BAO], total: 1, limit: 12, offset: 0 }),
    listAssignmentsByStore: () => Promise.resolve(ROWS),
    createAssignment: (...args: unknown[]) => createAssignment(...args),
    removeAssignment: (...args: unknown[]) => removeAssignment(...args),
    publishPermissions: vi.fn(),
  },
  ApiError: class ApiError extends Error {},
}));

/** Mounts the screen and waits for the store's assignments to arrive. */
async function mount() {
  render(() => <People />);
  await waitFor(() => expect(screen.getByText("Cam (C03)")).toBeTruthy());
}

/** Picks Bao in the assign search and the Cashier role, ready for "Where". */
async function chooseBaoAsCashier() {
  fireEvent.input(screen.getByPlaceholderText(messages["people.findEmployee"]!), {
    target: { value: "Bao" },
  });
  const match = await screen.findByRole("button", { name: "Bao (C02)" });
  fireEvent.click(match);
  fireEvent.change(screen.getByRole("combobox", { name: messages["people.role"]! }), {
    target: { value: CASHIER.role_template_id },
  });
}

describe("where an assignment reaches", () => {
  beforeEach(() => {
    localStorage.clear();
    vi.clearAllMocks();
    createAssignment.mockResolvedValue({ id: "01ASSIGNNEWAAAAAAAAAAAAAAA", stores: [] });
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

  it("shows each row's scope: this store, the group by name, or every store", async () => {
    await mount();
    const cell = (name: string) => screen.getByText(name).closest("tr")!;
    expect(within(cell("Alice (C01)")).getByText(messages["people.scope.store"]!)).toBeTruthy();
    expect(within(cell("Bao (C02)")).getByText(GROUP.name)).toBeTruthy();
    expect(within(cell("Cam (C03)")).getByText(messages["people.scope.tenant"]!)).toBeTruthy();
  });

  it("sends a group's id, and not the store's, when Where is a store group", async () => {
    await mount();
    await chooseBaoAsCashier();
    fireEvent.change(screen.getByRole("combobox", { name: messages["people.where"]! }), {
      target: { value: "group" },
    });
    fireEvent.change(screen.getByRole("combobox", { name: messages["people.storeGroup"]! }), {
      target: { value: GROUP.group_id },
    });
    fireEvent.click(screen.getByRole("button", { name: messages["people.assign"]! }));

    await waitFor(() => expect(createAssignment).toHaveBeenCalledTimes(1));
    expect(createAssignment).toHaveBeenCalledWith(
      TENANT.id,
      BAO.employee_id,
      { kind: "group", groupId: GROUP.group_id },
      CASHIER.role_template_id,
    );
  });

  it("sends the tenant-wide kind when Where is every store", async () => {
    await mount();
    await chooseBaoAsCashier();
    fireEvent.change(screen.getByRole("combobox", { name: messages["people.where"]! }), {
      target: { value: "tenant" },
    });
    fireEvent.click(screen.getByRole("button", { name: messages["people.assign"]! }));

    await waitFor(() => expect(createAssignment).toHaveBeenCalledTimes(1));
    expect(createAssignment.mock.calls[0]![2]).toEqual({ kind: "tenant" });
  });

  it("says a wider removal leaves every store it reached", async () => {
    await mount();
    const row = screen.getByText("Cam (C03)").closest("tr")!;
    const remove = within(row).getByRole("button", { name: messages["people.unassign"]! });
    // The screen is busy until its first load settles, and a busy screen's buttons are disabled.
    await waitFor(() => expect((remove as HTMLButtonElement).disabled).toBe(false));
    fireEvent.click(remove);
    expect(await screen.findByText(messages["people.unassignTenantMessage"]!)).toBeTruthy();
  });
});
