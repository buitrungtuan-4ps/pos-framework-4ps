// Publishing to a cohort, and the report that is the actual deliverable.
//
// A batch is deliberately not atomic (ADR-0122 §5), so the thing that must not go wrong on this
// screen is not the button — it is what the operator is told afterwards. Four properties are
// pinned here because each of them is a way the screen could look finished and lie:
//
//   * a `200` is shown as three counts and a row per shop, never as "done". A batch where two of
//     three shops were untouched and the screen said "published" is the failure this screen
//     exists to prevent;
//   * `skipped` and `failed` are drawn differently and carry the refusal's own sentence. Filing a
//     shop that was deliberately protected beside one that broke is the report going wrong rather
//     than the batch;
//   * a node whose value is typed per store copies it from a named shop, and refuses **before**
//     publishing when that shop has never published it — "copy nothing to fifty shops" reaching
//     the server would be a batch that succeeded at doing nothing;
//   * the three nodes ADR-0122 §4 excludes are not offerable. `locale` in that picker would be an
//     operator asking a question the server can only answer with a `400`.

import { cleanup, fireEvent, render, screen, waitFor } from "@solidjs/testing-library";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { StoreGroups } from "../src/screens/StoreGroups";
import { selectTenant } from "../src/state/session";
import en from "../src/i18n/en.json";

const messages = en as Record<string, string>;

const TENANT = { tenant_id: "01M22190WCY5PS7KCA7ET7H679", name: "Pizza 4P's Vietnam" };

const STORES = [
  { store_id: "01STOREAAAAAAAAAAAAAAAAAAA", name: "Bến Thành", brand_id: null, status: "active", etag: "v1" },
  { store_id: "01STOREBBBBBBBBBBBBBBBBBBB", name: "Xuân Thủy", brand_id: null, status: "active", etag: "v1" },
  { store_id: "01STORECCCCCCCCCCCCCCCCCCC", name: "Lê Thánh Tôn", brand_id: null, status: "active", etag: "v1" },
];

const GROUP = {
  group_id: "01GROUPAAAAAAAAAAAAAAAAAAA",
  tenant_id: TENANT.tenant_id,
  name: "Airport branches",
  status: "active",
  store_ids: STORES.map((store) => store.store_id),
  etag: "g1",
};

const MENU = {
  menu_id: "01MENUAAAAAAAAAAAAAAAAAAAA",
  tenant_id: TENANT.tenant_id,
  name: "Standard",
  parent_menu_id: null,
  status: "active",
  etag: "m1",
};

/** One batch over the three shops, in the three states ADR-0122 §6 distinguishes. */
const REPORT = {
  batch_id: "01BATCHAAAAAAAAAAAAAAAAAAA",
  group_id: GROUP.group_id,
  node: "menu",
  arguments: { menu_id: MENU.menu_id },
  actor_email: "ops@example.test",
  started_at_ms: 1_700_000_000_000,
  finished_at_ms: 1_700_000_001_000,
  applied: 1,
  skipped: 1,
  failed: 1,
  results: [
    {
      store_id: STORES[0]!.store_id,
      outcome: "applied",
      config_version_id: "01VERSIONAAAAAAAAAAAAAAAAA",
      at_ms: 1_700_000_000_100,
    },
    {
      store_id: STORES[1]!.store_id,
      outcome: "skipped",
      detail: "this store has no `tax` node yet; publish one to it before this",
      at_ms: 1_700_000_000_200,
    },
    {
      store_id: STORES[2]!.store_id,
      outcome: "failed",
      detail: "the menu names an archived item",
      at_ms: 1_700_000_000_300,
    },
  ],
};

const publishToStoreGroup = vi.fn();
const setStoreGroupMembers = vi.fn();
const readChannels = vi.fn();

vi.mock("../src/api/client", () => ({
  api: {
    listStoreGroups: () => Promise.resolve([GROUP]),
    listStores: () => Promise.resolve(STORES),
    listMenus: () => Promise.resolve([MENU]),
    capabilityCatalogue: () => Promise.resolve({ flags: [], presets: [], rules: [] }),
    listStoreGroupBatches: () => Promise.resolve([REPORT]),
    publishToStoreGroup: (...args: unknown[]) => publishToStoreGroup(...args),
    setStoreGroupMembers: (...args: unknown[]) => setStoreGroupMembers(...args),
    readChannels: (...args: unknown[]) => readChannels(...args),
    createStoreGroup: vi.fn(),
    updateStoreGroup: vi.fn(),
  },
  ApiError: class ApiError extends Error {},
}));

/** Mounts the screen and waits for the one cohort to arrive. */
async function mount() {
  render(() => <StoreGroups />);
  await waitFor(() => expect(screen.getByText(GROUP.name)).toBeTruthy());
}

/** Chooses `label` in the picker whose accessible name is `field`. */
function choose(field: string, label: string) {
  fireEvent.click(screen.getByRole("button", { name: field }));
  fireEvent.click(screen.getByRole("option", { name: label }));
}

describe("publishing to a store group", () => {
  beforeEach(() => {
    localStorage.clear();
    vi.clearAllMocks();
    publishToStoreGroup.mockResolvedValue(REPORT);
    selectTenant(TENANT.tenant_id, TENANT.name);
  });
  afterEach(cleanup);

  it("sends the node key and its arguments, and reports every shop", async () => {
    await mount();
    choose(messages["storeGroups.publishTo"]!, GROUP.name);
    choose(messages["storeGroups.menu"]!, MENU.name);
    fireEvent.click(screen.getByRole("button", { name: messages["storeGroups.publish"]! }));

    await waitFor(() => expect(publishToStoreGroup).toHaveBeenCalledTimes(1));
    expect(publishToStoreGroup).toHaveBeenCalledWith(TENANT.tenant_id, GROUP.group_id, "menu", {
      menu_id: MENU.menu_id,
    });

    // Every member is named, including the two that were not published to. The counts alone would
    // let a screen say "1 published" and leave the other two invisible.
    await waitFor(() => expect(screen.getByText(messages["storeGroups.report"]!)).toBeTruthy());
    for (const store of STORES) {
      expect(screen.getAllByText(store.name).length).toBeGreaterThan(0);
    }
  });

  it("draws a protected shop differently from a broken one, and quotes both refusals", async () => {
    await mount();
    choose(messages["storeGroups.publishTo"]!, GROUP.name);
    choose(messages["storeGroups.menu"]!, MENU.name);
    fireEvent.click(screen.getByRole("button", { name: messages["storeGroups.publish"]! }));
    await waitFor(() => expect(screen.getByText(messages["storeGroups.report"]!)).toBeTruthy());

    expect(screen.getAllByText(messages["storeGroups.outcome.skipped"]!).length).toBe(1);
    expect(screen.getAllByText(messages["storeGroups.outcome.failed"]!).length).toBe(1);
    expect(screen.getAllByText(messages["storeGroups.outcome.applied"]!).length).toBe(1);
    // The refusal's own sentence, not a code — it is the same one the single-store publish would
    // have returned, and it is what tells the operator to publish tax rates to that shop first.
    expect(screen.getByText(REPORT.results[1]!.detail!)).toBeTruthy();
    expect(screen.getByText(REPORT.results[2]!.detail!)).toBeTruthy();
  });

  it("offers only the nodes a batch can publish", async () => {
    await mount();
    fireEvent.click(screen.getByRole("button", { name: messages["storeGroups.node"]! }));
    const offered = screen.getAllByRole("option").map((node) => node.textContent);
    expect(offered).toHaveLength(13);
    // The three ADR-0122 §4 excludes, by the labels they would wear if anyone added them.
    expect(offered).not.toContain(messages["storeSettings.title"]);
    expect(offered).not.toContain(messages["nav.config"]);
  });

  it("refuses a copy-from node before publishing when the source has nothing to copy", async () => {
    readChannels.mockResolvedValue(null);
    await mount();
    choose(messages["storeGroups.publishTo"]!, GROUP.name);
    choose(messages["storeGroups.node"]!, messages["storeGroups.node.channels"]!);
    choose(messages["storeGroups.copyFrom"]!, STORES[0]!.name);
    fireEvent.click(screen.getByRole("button", { name: messages["storeGroups.publish"]! }));

    await waitFor(() =>
      expect(screen.getByText(messages["storeGroups.sourceHasNoNode"]!)).toBeTruthy(),
    );
    // The point of doing the read first: nothing reached the fan-out, so no shop was written to
    // with an empty document.
    expect(publishToStoreGroup).not.toHaveBeenCalled();
  });

  it("saves the whole membership under the version it was read at", async () => {
    setStoreGroupMembers.mockResolvedValue({ store_ids: [], etag: "g2" });
    await mount();
    fireEvent.click(screen.getByRole("button", { name: messages["storeGroups.editMembers"]! }));
    await waitFor(() => expect(screen.getByRole("dialog")).toBeTruthy());
    fireEvent.click(screen.getByRole("button", { name: messages["action.save"]! }));

    await waitFor(() => expect(setStoreGroupMembers).toHaveBeenCalledTimes(1));
    expect(setStoreGroupMembers).toHaveBeenCalledWith(
      TENANT.tenant_id,
      GROUP.group_id,
      GROUP.store_ids,
      GROUP.etag,
    );
  });
});
