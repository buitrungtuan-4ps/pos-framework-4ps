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

import { cleanup, fireEvent, render, screen, waitFor } from "@solidjs/testing-library";
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
const createBrand = vi.fn();

vi.mock("../src/api/client", () => ({
  api: {
    listStores: () => listStores(),
    listBrands: () => listBrands(),
    listTenants: () => listTenants(),
    createStore: (...args: unknown[]) => createStore(...args),
    createBrand: (...args: unknown[]) => createBrand(...args),
    updateStore: vi.fn(),
    updateBrand: vi.fn(),
    updateTenant: vi.fn(),
  },
  ApiError: class ApiError extends Error {},
}));

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

  it("opens the store form from the list header, and closes it again", async () => {
    mountStores();
    await waitFor(() => expect(screen.getByText(STORE.name)).toBeTruthy());

    fireEvent.click(screen.getByRole("button", { name: "Create a store" }));

    expect(screen.getByRole("dialog")).toBeTruthy();
    expect(screen.getByLabelText("Name")).toBeTruthy();

    fireEvent.click(screen.getByRole("button", { name: "Cancel" }));

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
    fireEvent.click(screen.getByRole("button", { name: "Create a store" }));

    fireEvent.click(screen.getByRole("button", { name: "Create" }));

    expect(createStore).not.toHaveBeenCalled();
    expect(screen.getByRole("dialog")).toBeTruthy();
  });

  it("creates a store with the brand chosen in the same form", async () => {
    createStore.mockResolvedValue({});
    mountStores();
    await waitFor(() => expect(screen.getByText(STORE.name)).toBeTruthy());
    fireEvent.click(screen.getByRole("button", { name: "Create a store" }));

    fireEvent.input(screen.getByLabelText("Name"), { target: { value: "4P's Ben Thanh" } });
    fireEvent.change(screen.getByLabelText("Brand"), { target: { value: BRAND.brand_id } });
    fireEvent.click(screen.getByRole("button", { name: "Create" }));

    await waitFor(() =>
      expect(createStore).toHaveBeenCalledWith(
        TENANT.tenant_id,
        "4P's Ben Thanh",
        BRAND.brand_id,
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
