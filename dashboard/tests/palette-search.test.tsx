// The command palette searches the tenant's things, not only its screens (Wave 4 · PR-8, F16).
//
// The palette's own header promised this and F2 did not deliver it, so a console whose search box
// only found its own screens sent an operator who knew the shop's name to a list to scroll. What is
// worth pinning is not the markup but the four rules that make the search either useful or a leak:
//
//   * a shop opens *on that shop* — choosing it sets the working store, or the hub opens on whoever
//     was selected before and reads as the wrong answer to the question that was asked;
//   * an item hands its name to the catalogue's own search box, because the console has no route to
//     a single item and a dead link is worse than a filter;
//   * a one-letter query asks the server nothing — it would match most of the master;
//   * the employee roster is never searched. A name in a roster is personal data, and a global
//     search box is the one place in the console where it would be typed casually.

import { MemoryRouter, Route } from "@solidjs/router";
import { cleanup, fireEvent, render, screen, waitFor } from "@solidjs/testing-library";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { CommandPalette, openPalette } from "../src/components/CommandPalette";
import { selectStore, selectTenant, storeId } from "../src/state/session";

const TENANT = { tenant_id: "01M22190WCY5PS7KCA7ET7H679", name: "Pizza 4P's Vietnam" };

const STORES = [
  { store_id: "01STOREAAAAAAAAAAAAAAAAAAA", name: "Bến Thành", brand_id: null, status: "active", etag: "v1" },
  { store_id: "01STOREBBBBBBBBBBBBBBBBBBB", name: "Ginza", brand_id: null, status: "archived", etag: "v1" },
];

const ITEM = {
  menu_item_id: "01ITEMAAAAAAAAAAAAAAAAAAAA",
  tenant_id: TENANT.tenant_id,
  name: "Phở bò",
  name_translations: {},
  tax_class_id: "01TAXAAAAAAAAAAAAAAAAAAAAA",
  item_category_id: null,
  item_subcategory_id: null,
  image_ref: null,
  status: "active",
  etag: "v1",
};

const listStores = vi.fn(() => Promise.resolve(STORES));
const listItemsPage = vi.fn(() => Promise.resolve({ items: [ITEM], total: 1 }));

vi.mock("../src/api/client", () => ({
  api: {
    listStores: (...args: unknown[]) => listStores(...(args as [])),
    listItemsPage: (...args: unknown[]) => listItemsPage(...(args as [])),
  },
  ApiError: class ApiError extends Error {},
}));

function mount() {
  render(() => (
    <MemoryRouter>
      <Route path="*" component={CommandPalette} />
    </MemoryRouter>
  ));
  openPalette();
  return screen.getByRole("combobox");
}

beforeEach(() => {
  listStores.mockClear();
  listItemsPage.mockClear();
  selectTenant(TENANT.tenant_id, TENANT.name);
  selectStore("", "");
});

afterEach(cleanup);

describe("searching the tenant's things from the palette", () => {
  it("offers a shop by name and opens the hub on it", async () => {
    const field = mount();
    fireEvent.input(field, { target: { value: "Bến" } });

    const row = await screen.findByRole("option", { name: /Bến Thành/ });
    fireEvent.click(row);
    // The working store is set before the jump, which is what makes the hub answer about this shop
    // rather than about whichever one was last open (ADR-0120).
    expect(storeId()).toBe(STORES[0]!.store_id);
  });

  it("does not offer an archived shop", async () => {
    const field = mount();
    fireEvent.input(field, { target: { value: "Ginza" } });

    await waitFor(() => expect(listStores).toHaveBeenCalled());
    expect(screen.queryByRole("option", { name: /Ginza/ })).toBeNull();
  });

  it("hands an item's name to the catalogue's search box", async () => {
    const field = mount();
    fireEvent.input(field, { target: { value: "Phở" } });

    const row = await screen.findByRole("option", { name: /Phở bò/ });
    // The console has no route to one item, so the palette filters the catalogue instead of
    // inventing a URL nothing serves.
    expect(row.closest("li")).toBeTruthy();
    await waitFor(() => expect(listItemsPage).toHaveBeenCalled());
    const [, , filter] = listItemsPage.mock.calls[0] as unknown as [string, unknown, { q: string }];
    expect(filter.q).toBe("Phở");
  });

  it("asks the server nothing for a single letter", async () => {
    const field = mount();
    fireEvent.input(field, { target: { value: "p" } });

    // Long enough that a debounced call would have fired.
    await new Promise((resolve) => setTimeout(resolve, 400));
    expect(listStores).not.toHaveBeenCalled();
    expect(listItemsPage).not.toHaveBeenCalled();
  });

  it("never searches the employee roster", async () => {
    // The mocked client has no `listEmployees` at all: if the palette ever reached for one, this
    // test would fail with a TypeError rather than quietly start leaking names.
    const field = mount();
    fireEvent.input(field, { target: { value: "Nguyễn" } });

    await waitFor(() => expect(listStores).toHaveBeenCalled());
    expect(screen.queryByRole("option", { name: /Nguyễn/ })).toBeNull();
  });
});
