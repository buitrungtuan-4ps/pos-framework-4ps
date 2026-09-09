// The checklist renders on the screen a fresh console actually opens (Wave 3 · Stage 2).
//
// The rules are pinned in `get-started.test.ts`. What is checked here is the wiring the rules
// cannot see: that the panel appears on the landing screen with no context, that its links carry
// the working context so they land on the right store, that it stands down once every required step
// is done, and that it fires no store-scoped read before a store is chosen — the last being the
// reason the panel is safe to put on a screen every operator opens all day.

import { MemoryRouter, Route } from "@solidjs/router";
import { cleanup, render, screen, waitFor } from "@solidjs/testing-library";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { GetStarted } from "../src/components/GetStarted";
import { setStoreId, setTenantId } from "../src/state/session";

const TENANT = "01M22190WCY5PS7KCA7ET7H679";
const STORE = "01M2219QK4T3W6Z0Y8FBQ2X5MV";

const listTenants = vi.fn();
const listBrands = vi.fn();
const listStores = vi.fn();
const listApiKeys = vi.fn();
const configVersions = vi.fn();
const admittedDevices = vi.fn();
const fleetStore = vi.fn();

vi.mock("../src/api/client", () => ({
  api: {
    listTenants: () => listTenants(),
    listBrands: (tenant: string) => listBrands(tenant),
    listStores: (tenant: string) => listStores(tenant),
    listApiKeys: (tenant: string) => listApiKeys(tenant),
    configVersions: (tenant: string, store: string) => configVersions(tenant, store),
    admittedDevices: (tenant: string, store: string) => admittedDevices(tenant, store),
    fleetStore: (tenant: string, store: string) => fleetStore(tenant, store),
  },
  ApiError: class ApiError extends Error {},
}));

function mount() {
  render(() => (
    <MemoryRouter>
      <Route path="/" component={GetStarted} />
    </MemoryRouter>
  ));
}

/** Everything answered, everything done — the state in which the checklist should disappear. */
function setUpFully() {
  listTenants.mockResolvedValue([{ tenant_id: TENANT }]);
  listBrands.mockResolvedValue([{ brand_id: "b" }]);
  listStores.mockResolvedValue([{ store_id: STORE }]);
  listApiKeys.mockResolvedValue([{ id: "k", store_id: STORE, revoked: false }]);
  configVersions.mockResolvedValue([{ version_id: "v1" }]);
  admittedDevices.mockResolvedValue([{ local_device_id: "d" }]);
  fleetStore.mockResolvedValue({ last_seen_at_ms: 1_757_000_000_000 });
}

beforeEach(() => {
  localStorage.clear();
  setTenantId("");
  setStoreId("");
  listTenants.mockResolvedValue([]);
  listBrands.mockResolvedValue([]);
  listStores.mockResolvedValue([]);
  listApiKeys.mockResolvedValue([]);
  configVersions.mockResolvedValue([]);
  admittedDevices.mockResolvedValue([]);
  fleetStore.mockResolvedValue({ last_seen_at_ms: null });
});

afterEach(cleanup);

describe("on a console with nothing in it", () => {
  it("names the first step and reports no progress", async () => {
    mount();
    await waitFor(() => expect(listTenants).toHaveBeenCalled());
    expect(screen.getByText("Create an organisation")).toBeTruthy();
    expect(screen.getByText("0 of 6 required steps done.")).toBeTruthy();
  });

  it("asks for nothing scoped to a store, because no store is chosen", async () => {
    mount();
    await waitFor(() => expect(listTenants).toHaveBeenCalled());
    for (const read of [listApiKeys, configVersions, admittedDevices, fleetStore]) {
      expect(read).not.toHaveBeenCalled();
    }
    // Nor tenant-scoped: `onScopedContext` gates on the context being satisfied, so an empty id is
    // never sent — the whole reason the `… is not a ULID` error class is gone from this console.
    expect(listBrands).not.toHaveBeenCalled();
    expect(listStores).not.toHaveBeenCalled();
  });
});

describe("with a tenant and a store chosen", () => {
  it("reads each step's own answer and links to the screen that does it", async () => {
    setTenantId(TENANT);
    setStoreId(STORE);
    mount();
    await waitFor(() => expect(fleetStore).toHaveBeenCalledWith(TENANT, STORE));
    // The reads that were still `waiting` a moment ago now carry the real context.
    expect(listBrands).toHaveBeenCalledWith(TENANT);
    expect(configVersions).toHaveBeenCalledWith(TENANT, STORE);
    const link = screen.getByRole("link", { name: "Open activation" });
    expect(link.getAttribute("href")).toBe(`/t/${TENANT}/activation?store=${STORE}`);
  });

  it("counts the store's own key and not a tenant-wide one", async () => {
    setTenantId(TENANT);
    setStoreId(STORE);
    listApiKeys.mockResolvedValue([
      { id: "integration", store_id: null, revoked: false },
      { id: "revoked", store_id: STORE, revoked: true },
    ]);
    mount();
    await waitFor(() => expect(listApiKeys).toHaveBeenCalled());
    await waitFor(() => expect(screen.getByText("Issue the store's key")).toBeTruthy());
    // Two keys exist and neither one is a live key bound to this store, so the step is not done.
    expect(screen.queryByText("6 of 6 required steps done.")).toBeNull();
  });
});

describe("once the setup is finished", () => {
  it("stands down, so the landing screen is not a checklist forever", async () => {
    setUpFully();
    setTenantId(TENANT);
    setStoreId(STORE);
    mount();
    await waitFor(() => expect(fleetStore).toHaveBeenCalled());
    await waitFor(() => expect(screen.queryByText("Set up this cloud")).toBeNull());
  });

  it("stays up when a read was refused, rather than declaring victory it cannot see", async () => {
    setUpFully();
    admittedDevices.mockRejectedValue(new Error("the caller is not permitted"));
    setTenantId(TENANT);
    setStoreId(STORE);
    mount();
    await waitFor(() => expect(admittedDevices).toHaveBeenCalled());
    await waitFor(() => expect(screen.getByText("Could not check")).toBeTruthy());
    expect(screen.getByText("Set up this cloud")).toBeTruthy();
  });
});
