// A store the wizard creates is given the owner's new-store values (ADR-0160 decision 1).
//
// Today that is one value: a new store refuses to sell while no shift is open, where an existing
// store keeps selling until somebody changes it (the owner, 2026-09-30). The wizard asks the cloud
// for it the moment the store exists, and three properties are pinned, because each is a way the
// wizard could quietly get it wrong:
//
//   * it asks for the store it **just created**, once the store exists — never before, and never for
//     whatever store was in context when the wizard opened;
//   * a failure does not undo or hold up the store: the wizard has moved on to the key, says the
//     store exists, and links to Shared settings for that store, where the values can be given again;
//   * a store whose values were saved but whose publish failed is told so too, rather than being
//     told it has them.

import { MemoryRouter, Route } from "@solidjs/router";
import { cleanup, fireEvent, render, screen, waitFor } from "@solidjs/testing-library";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { ApiError } from "../src/api/client";
import { NewStore } from "../src/screens/NewStore";
import { selectStore, selectTenant } from "../src/state/session";

const TENANT = { id: "01M22190WCY5PS7KCA7ET7H679", name: "Pizza 4P's Vietnam" };
const CREATED = {
  store_id: "01M2219QK4T3W6Z0Y8FBQ2X5MV",
  tenant_id: TENANT.id,
  brand_id: null,
  name: "Xuân Thủy",
  status: "active",
  etag: "s1",
};
/** The store in context before the wizard ran — the one nothing here may be aimed at. */
const OTHER_STORE = { id: "01M221A7XN6R4H9V2C0KDPJ8ZE", name: "Bến Thành" };

const createStore = vi.fn();
const applySettingPresets = vi.fn();

vi.mock("../src/api/client", () => ({
  api: {
    listBrands: () => Promise.resolve([]),
    createStore: (...args: unknown[]) => createStore(...args),
    applySettingPresets: (...args: unknown[]) => applySettingPresets(...args),
  },
  // The client's own shape, so a refusal reads as its message the way the real one does.
  ApiError: class ApiError extends Error {
    readonly status: number;
    constructor(status: number, message: string) {
      super(message);
      this.status = status;
    }
  },
}));

function mountWizard() {
  render(() => (
    <MemoryRouter>
      <Route path="/" component={NewStore} />
    </MemoryRouter>
  ));
}

/** Names the store and presses Next, which is what creates it. */
function createTheStore() {
  fireEvent.input(screen.getByLabelText(/^Name/), { target: { value: CREATED.name } });
  fireEvent.click(screen.getByRole("button", { name: "Next" }));
}

describe("a store the wizard creates", () => {
  beforeEach(() => {
    localStorage.clear();
    vi.clearAllMocks();
    selectTenant(TENANT.id, TENANT.name);
    selectStore(OTHER_STORE.id, OTHER_STORE.name);
    createStore.mockResolvedValue(CREATED);
  });
  afterEach(cleanup);

  it("is given the new-store values once it exists, and says which", async () => {
    applySettingPresets.mockResolvedValue({
      applied: ["shift.no_shift_selling"],
      stores: [{ store_id: CREATED.store_id, outcome: "SETTING_PUBLISH_APPLIED" }],
    });
    mountWizard();
    createTheStore();

    await waitFor(() => expect(applySettingPresets).toHaveBeenCalledTimes(1));
    expect(applySettingPresets).toHaveBeenCalledWith(TENANT.id, CREATED.store_id);
    // The store came first: the values are given to a store that exists.
    expect(createStore.mock.invocationCallOrder[0]).toBeLessThan(
      applySettingPresets.mock.invocationCallOrder[0]!,
    );
    expect(
      await screen.findByText("Gave this store the new-store setting: Selling with no shift open."),
    ).toBeTruthy();
    expect(screen.getByText("Store created.")).toBeTruthy();
  });

  it("carries on when the values cannot be given, and says where to finish", async () => {
    applySettingPresets.mockRejectedValue(new ApiError(503, "the settings store is unreachable"));
    mountWizard();
    createTheStore();

    expect(
      await screen.findByText(
        "The store is created, but its new-store settings could not be applied: the settings store is unreachable",
      ),
    ).toBeTruthy();
    // Not undone and not held: the wizard is on the key step for the store it made.
    expect(screen.getByText("Store created.")).toBeTruthy();
    expect(screen.getByRole("button", { name: "Issue API key" })).toBeTruthy();
    expect(createStore).toHaveBeenCalledTimes(1);
    // The link carries the new store, not the one that was in context.
    const retry = screen.getByRole("link", { name: "Open Shared settings for this store" });
    expect(retry.getAttribute("href")).toBe(`/t/${TENANT.id}/settings?store=${CREATED.store_id}`);
  });

  it("says so when the values were saved but the store could not be published to", async () => {
    applySettingPresets.mockResolvedValue({
      applied: ["shift.no_shift_selling"],
      stores: [{ store_id: CREATED.store_id, outcome: "SETTING_PUBLISH_FAILED" }],
    });
    mountWizard();
    createTheStore();

    expect(
      await screen.findByText(
        "The new-store settings are saved, but they could not be published to the store yet. Publish the store again from Shared settings.",
      ),
    ).toBeTruthy();
    expect(
      screen.getByRole("link", { name: "Open Shared settings for this store" }).getAttribute("href"),
    ).toBe(`/t/${TENANT.id}/settings?store=${CREATED.store_id}`);
  });

  it("is not asked for when the store could not be created", async () => {
    createStore.mockRejectedValue(new ApiError(409, "a store with that name exists"));
    mountWizard();
    createTheStore();

    await waitFor(() => expect(createStore).toHaveBeenCalledTimes(1));
    expect(await screen.findByText("a store with that name exists")).toBeTruthy();
    expect(applySettingPresets).not.toHaveBeenCalled();
  });
});
