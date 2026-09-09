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
