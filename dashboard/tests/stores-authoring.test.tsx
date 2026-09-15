// Stores, on the authoring kit — the screen from the owner's report
// ([ADR-0121](../../docs/adr/0121-one-way-to-author-an-entity.md) §6).
//
// The complaint was that adding a record "hiện nguyên cái card bự ra": the screen ended in two
// permanently-visible cards, "Create store" and "Create brand", occupying the bottom half of the
// page whether or not anyone wanted to create anything. So the property to pin is not "a panel can
// open" — the kit's own tests cover that — it is that **nothing asks for input until an operator
// asks for it**, and that the two lists no longer freeze each other.
//
// A screen test rather than a kit test because both are emergent: they come from how Stores wires
// the kit up, and a future edit could put a form back on the page without touching `FormPanel`.
//
// Wave 4 · PR-4 adds the other half of §6, decision D4: **there is one way to create a store and it
// is the wizard**. The header's button is a link now, and the panel this file exercises is the edit
// panel — so what is pinned below is that the create path leaves the screen and the edit path still
// asks before it writes.

import { cleanup, fireEvent, render, screen, waitFor, within } from "@solidjs/testing-library";
import { MemoryRouter, Route } from "@solidjs/router";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { Stores } from "../src/screens/Stores";
import { selectTenant } from "../src/state/session";

const TENANT = { tenant_id: "01M22190WCY5PS7KCA7ET7H679", name: "Pizza 4P's Vietnam" };
const STORE = {
  store_id: "01M221BB8BB5SESQDB895SJHJS",
  name: "4P's Le Thanh Ton",
  brand_id: null,
  status: "active",
  etag: "v1",
};
const BRAND = {
  brand_id: "01M221CCCCCCCCCCCCCCCCCCCC",
  name: "Pizza 4P's",
  status: "active",
  etag: "v1",
};

const listStores = vi.fn();
const listBrands = vi.fn();
const listTenants = vi.fn();
const createStore = vi.fn();
const updateStore = vi.fn();
const createBrand = vi.fn();

vi.mock("../src/api/client", () => ({
  api: {
    listStores: () => listStores(),
    listBrands: () => listBrands(),
    listTenants: () => listTenants(),
    createStore: (...args: unknown[]) => createStore(...args),
    createBrand: (...args: unknown[]) => createBrand(...args),
    updateStore: (...args: unknown[]) => updateStore(...args),
    updateBrand: vi.fn(),
    updateTenant: vi.fn(),
  },
  ApiError: class ApiError extends Error {},
}));

/**
 * Open the store row's edit panel.
 *
 * Scoped to the row that names the store, because the brands table below carries its own Edit: the
 * row's verbs sit behind a kebab now (Wave 4 · PR-3, V19), so the click is one step further in and
 * "the first Edit on the page" stopped being an answer.
 */
function openEdit() {
  const row = screen.getByText(STORE.name).closest("tr");
  if (!row) {
    throw new Error("the store row is not on the page");
  }
  fireEvent.click(within(row).getByRole("button", { name: "Actions" }));
  fireEvent.click(within(row).getByRole("button", { name: "Edit" }));
}

function mountStores() {
  return render(() => (
    <MemoryRouter>
      <Route path="/" component={Stores} />
    </MemoryRouter>
  ));
}

describe("authoring a store", () => {
  beforeEach(() => {
    localStorage.clear();
    vi.clearAllMocks();
    listStores.mockResolvedValue([STORE]);
    listBrands.mockResolvedValue([BRAND]);
    listTenants.mockResolvedValue([{ ...TENANT, status: "active", etag: "v1" }]);
    selectTenant(TENANT.tenant_id, TENANT.name);
  });
  afterEach(cleanup);

  // The report, as a property.
  it("shows no form until the operator asks for one", async () => {
    mountStores();
    await waitFor(() => expect(screen.getByText(STORE.name)).toBeTruthy());

    expect(screen.queryByRole("dialog")).toBeNull();
    // The store's own name field belongs to the panel; the only text input on the page at rest is
    // the organisation rename, which is a settings row rather than a create form.
    expect(screen.queryByLabelText("Name")).toBeNull();
    expect(screen.queryByLabelText("Brand name")).toBeNull();
  });

  // D4. The screen offered two: a panel that wrote the registry row and stopped, and the wizard
  // beside it. A store created by the panel had no key and no installer, so it could not trade, and
  // nothing on the screen said so.
  it("sends 'Create a store' to the wizard rather than opening a second create form", async () => {
    mountStores();
    await waitFor(() => expect(screen.getByText(STORE.name)).toBeTruthy());

    const create = screen.getByRole("link", { name: "Create a store" });
    expect(create.getAttribute("href")).toBe(`/t/${TENANT.tenant_id}/stores/new`);
    // And there is no second button that looks like it.
    expect(screen.queryByRole("button", { name: "Create a store" })).toBeNull();
    expect(screen.queryByRole("dialog")).toBeNull();
  });

  it("opens the brand form from the brands header", async () => {
    mountStores();
    await waitFor(() => expect(screen.getByText(BRAND.name)).toBeTruthy());

    fireEvent.click(screen.getByRole("button", { name: "Create a brand" }));

    expect(screen.getByLabelText("Brand name")).toBeTruthy();
  });

  it("refuses an empty name in the panel rather than calling the API", async () => {
    mountStores();
    await waitFor(() => expect(screen.getByText(STORE.name)).toBeTruthy());
    openEdit();

    const panel = screen.getByRole("dialog");
    fireEvent.input(screen.getByLabelText("Name"), { target: { value: "  " } });
    fireEvent.click(within(panel).getByRole("button", { name: "Save" }));

    expect(updateStore).not.toHaveBeenCalled();
    expect(screen.getByRole("dialog")).toBeTruthy();
  });

  it("renames and re-brands a store from the same panel, carrying its version", async () => {
    updateStore.mockResolvedValue({});
    mountStores();
    await waitFor(() => expect(screen.getByText(STORE.name)).toBeTruthy());
    openEdit();

    const panel = screen.getByRole("dialog");
    fireEvent.input(screen.getByLabelText("Name"), { target: { value: "4P's Ben Thanh" } });
    fireEvent.change(screen.getByLabelText("Brand"), { target: { value: BRAND.brand_id } });
    fireEvent.click(within(panel).getByRole("button", { name: "Save" }));

    await waitFor(() =>
      expect(updateStore).toHaveBeenCalledWith(
        STORE.store_id,
        TENANT.tenant_id,
        { name: "4P's Ben Thanh", status: "active", brandId: BRAND.brand_id },
        STORE.etag,
      ),
    );
  });

  // The brand column used to be a `<select>` in the cell that wrote on `change`: one mis-click moved
  // a shop to another brand, with no confirmation and no undo. It is a label now.
  it("does not reassign a brand from the table row", async () => {
    mountStores();
    await waitFor(() => expect(screen.getByText(STORE.name)).toBeTruthy());

    // The only `Brand` control on the page at rest would be that cell select; the panel's one is
    // not mounted yet.
    expect(screen.queryByLabelText("Brand")).toBeNull();
  });
});
