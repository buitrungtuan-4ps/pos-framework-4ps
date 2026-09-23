// Creating the first tenant has to move the address bar (Wave 3 · D3).
//
// The tenant is a path segment precisely so a console link carries the context it was read under —
// shareable, bookmarkable, and two tabs on two tenants. Every way of *choosing* a tenant honoured
// that except the way a fresh install starts: `createTenant` selected the new tenant and loaded its
// stores but never called `goToContext`, so the very first action on a new cell left the URL on the
// pre-context landing while the breadcrumb, the nav and every screen behaved as though a tenant was
// chosen.
//
// Everything here is imported statically and the context is reset through the session module's own
// setters. `vi.resetModules()` plus a dynamic import would give the picker a *second* instance of
// `@solidjs/router`, whose context is not the one `MemoryRouter` provides, and the component throws
// "router primitives can be only used inside a Route" for a reason that has nothing to do with the
// code under test.

import { createMemoryHistory, MemoryRouter, Route } from "@solidjs/router";
import { cleanup, fireEvent, render, screen, waitFor } from "@solidjs/testing-library";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { ContextPicker } from "../src/components/ContextPicker";
import { setStoreId, setTenantId, tenantId, tenantName } from "../src/state/session";

const TENANT = { tenant_id: "01M22190WCY5PS7KCA7ET7H679", name: "Pizza 4P's Vietnam" };
const OTHER = { tenant_id: "01M2219YDKJ4Q2X5N0R8BW3T61", name: "Pizza 4P's Japan" };

const listTenants = vi.fn();
const listStores = vi.fn();
const createTenant = vi.fn();

vi.mock("../src/api/client", () => ({
  api: {
    listTenants: () => listTenants(),
    listStores: (tenant: string) => listStores(tenant),
    createTenant: (name: string) => createTenant(name),
  },
  ApiError: class ApiError extends Error {},
}));

/**
 * Mounts the picker on the pre-context landing — the screen a fresh install actually opens on — and
 * hands back the router's history, which is where the address bar lives here. `MemoryRouter` never
 * touches `window.location`, so asserting on that would pass no matter what the component did.
 */
function mountPicker() {
  const history = createMemoryHistory();
  render(() => (
    <MemoryRouter history={history}>
      <Route path="/" component={ContextPicker} />
      <Route path="/t/:tenant" component={ContextPicker} />
    </MemoryRouter>
  ));
  return history;
}

/** Open the picker, name the first tenant, and create it — the operator's three actions. */
async function createFirstTenant() {
  fireEvent.click(screen.getByLabelText("Change tenant or store"));
  await waitFor(() => expect(listTenants).toHaveBeenCalled());
  fireEvent.input(screen.getByLabelText("New tenant name"), { target: { value: TENANT.name } });
  fireEvent.click(screen.getByRole("button", { name: "Create" }));
  await waitFor(() => expect(createTenant).toHaveBeenCalledWith(TENANT.name));
}

beforeEach(() => {
  localStorage.clear();
  setTenantId("");
  setStoreId("");
  listTenants.mockResolvedValue([]);
  listStores.mockResolvedValue([]);
  createTenant.mockResolvedValue(TENANT);
});

afterEach(cleanup);

describe("the context picker on a fresh install", () => {
  it("puts the tenant it just created into the address bar", async () => {
    const history = mountPicker();
    expect(history.get()).toBe("/");

    await createFirstTenant();

    await waitFor(() => {
      expect(history.get()).toBe(`/t/${TENANT.tenant_id}`);
    });
  });

  it("selects the new tenant so every screen can read it", async () => {
    mountPicker();
    expect(tenantId()).toBe("");

    await createFirstTenant();

    await waitFor(() => expect(tenantId()).toBe(TENANT.tenant_id));
    expect(tenantName()).toBe(TENANT.name);
  });
});

// Opening the picker on a cell that already has tenants — the everyday case, not the fresh install
// above.
describe("the context picker on a cell that already has tenants", () => {
  beforeEach(() => {
    listTenants.mockResolvedValue([TENANT, OTHER]);
  });

  it("puts the cursor in the search box the first time it is opened", async () => {
    // The first open is the whole test. On it the list is still in flight, so the box is not in the
    // DOM yet and the skeleton is — focus logic that waits only on the panel being open reaches for
    // a ref that is still `undefined`, silently focuses nothing, and never runs again. The picker
    // then focuses correctly from the *second* open onward, which is the one nobody notices is
    // broken. Asserting on a reopen would pass against exactly that bug.
    mountPicker();
    fireEvent.click(screen.getByLabelText("Change tenant or store"));

    const box = await screen.findByLabelText("Search…");
    await waitFor(() => expect(document.activeElement).toBe(box));
  });

  it("announces the panel as a dialog the trigger opens", async () => {
    // Without these the panel is an unnamed `div`: a screen reader reads its contents with nothing
    // to say what opened or what it is for.
    mountPicker();
    const trigger = screen.getByLabelText("Change tenant or store");
    expect(trigger.getAttribute("aria-haspopup")).toBe("dialog");

    fireEvent.click(trigger);

    // Asked for by its accessible name, so the assertion is what a screen reader would announce
    // rather than which attribute happens to carry it.
    expect(await screen.findByRole("dialog", { name: "Change tenant or store" })).toBeDefined();
  });
});
