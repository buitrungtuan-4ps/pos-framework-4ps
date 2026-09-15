// The publish centre, and the two things it must never soften
// ([ADR-0125](../../docs/adr/0125-a-release-is-one-decision-many-writes.md)).
//
// A release is one decision and many writes, and both halves can go wrong on this screen in a way
// that looks fine:
//
//   * **The report must show every pair, and a failed pair must carry its reason.** A release that
//     could only read "done" or "failed" would be useless at forty stores: what an operator needs at
//     04:05 on Monday is the two shops that did not take it. A pair that failed is still `pending` —
//     the activator retries it — so the screen has to show the reason beside a neutral badge rather
//     than drawing a retry as a loss.
//   * **The two moments must stay two.** "Monday 04:00, local" and "one instant in UTC" are
//     different questions, and a screen that sent both would be asking the server for a refusal. A
//     store with no published timezone can only take the second, which is why the first exists as a
//     separate field rather than as a checkbox on the same one.
//
// Also pinned: `partial` is drawn as danger. `applied` is the only unqualified good, and a release
// that shipped to thirty-eight of forty shops is not one of them.

import { cleanup, fireEvent, render, screen, waitFor } from "@solidjs/testing-library";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { Releases } from "../src/screens/Releases";
import { selectTenant } from "../src/state/session";
import en from "../src/i18n/en.json";

const messages = en as Record<string, string>;

const TENANT = { tenant_id: "01M22190WCY5PS7KCA7ET7H679", name: "Pizza 4P's Vietnam" };

const STORES = [
  { store_id: "01STOREAAAAAAAAAAAAAAAAAAA", name: "Bến Thành", brand_id: null, status: "active", etag: "v1" },
  { store_id: "01STOREBBBBBBBBBBBBBBBBBBB", name: "Ginza", brand_id: null, status: "active", etag: "v1" },
];

const GROUP = {
  group_id: "01GROUPAAAAAAAAAAAAAAAAAAA",
  tenant_id: TENANT.tenant_id,
  name: "Tết cohort",
  status: "active",
  store_ids: STORES.map((store) => store.store_id),
  etag: "g1",
};

const RELEASE = {
  release_id: "01RELEASEAAAAAAAAAAAAAAAAA",
  tenant_id: TENANT.tenant_id,
  name: "Tết 2027",
  status: "partial",
  target_group_id: GROUP.group_id,
  wall_clock_date: "2027-02-08",
  wall_clock_time: "04:00",
  instant_at_ms: null,
  created_by: "01ADMINAAAAAAAAAAAAAAAAAAA",
  created_at_ms: 1_700_000_000_000,
  updated_at_ms: 1_700_000_100_000,
};

/**
 * One release's pairs, in the three states the grid must tell apart — and the one that is easiest
 * to get wrong: a pair that is still `pending` *and* carries the reason it last could not apply.
 */
const REPORT = {
  release: RELEASE,
  pairs: [
    {
      pair_id: "01PAIRAAAAAAAAAAAAAAAAAAAA",
      store_id: STORES[0]!.store_id,
      node: "menu",
      status: "applied",
      effective_at_ms: 1_800_000_000_000,
      applied_version_id: "01VERSIONAAAAAAAAAAAAAAAAA",
      failure: null,
    },
    {
      pair_id: "01PAIRBBBBBBBBBBBBBBBBBBBB",
      store_id: STORES[1]!.store_id,
      node: "menu",
      status: "pending",
      effective_at_ms: 1_800_000_007_200,
      applied_version_id: null,
      failure: "the menu names an archived item",
    },
  ],
};

const createRelease = vi.fn();
const scheduleRelease = vi.fn();
const readRelease = vi.fn();

vi.mock("../src/api/client", () => ({
  api: {
    listReleases: () => Promise.resolve([RELEASE]),
    listStoreGroups: () => Promise.resolve([GROUP]),
    listStores: () => Promise.resolve(STORES),
    listMenus: () => Promise.resolve([]),
    readRelease: (...args: unknown[]) => readRelease(...args),
    createRelease: (...args: unknown[]) => createRelease(...args),
    scheduleRelease: (...args: unknown[]) => scheduleRelease(...args),
    cancelRelease: vi.fn(),
  },
  ApiError: class ApiError extends Error {},
}));

/** Mounts the screen and waits for the one release to arrive. */
async function mount() {
  render(() => <Releases />);
  await waitFor(() => expect(screen.getByText(RELEASE.name)).toBeTruthy());
}

describe("the publish centre", () => {
  beforeEach(() => {
    vi.clearAllMocks();
    readRelease.mockResolvedValue(REPORT);
    selectTenant(TENANT.tenant_id, TENANT.name);
  });
  afterEach(cleanup);

  it("shows a release aimed at a cohort, at the moment the operator said", async () => {
    await mount();

    // Not an instant: the operator said "04:00, local", and forty shops will each reach that hour
    // at their own moment. Printing one converted timestamp here would be the screen deciding which
    // shop's clock is the real one.
    expect(screen.getByText(/2027-02-08.*04:00/)).toBeTruthy();
    expect(screen.getByText(new RegExp(GROUP.name))).toBeTruthy();
  });

  it("draws a partly-done release as a failure, not as done", async () => {
    await mount();

    // `applied` is the only unqualified good. A release that reached thirty-eight of forty shops is
    // the case `partial` exists for, and the badge must not read like a success.
    const badge = screen.getByText(messages["releases.status.partial"]!);
    expect(badge.className).toContain("danger");
  });

  it("reports every pair, and a pair that could not apply carries why", async () => {
    await mount();
    fireEvent.click(screen.getByRole("button", { name: messages["releases.viewReport"]! }));

    await waitFor(() =>
      expect(screen.getByText("the menu names an archived item")).toBeTruthy(),
    );
    // Both shops, named — the grid is the deliverable, not the button.
    expect(screen.getByText(STORES[0]!.name)).toBeTruthy();
    expect(screen.getByText(STORES[1]!.name)).toBeTruthy();
    // The failed pair is still waiting, because the activator retries it. Drawing it as lost would
    // send an operator to fix by hand what the next tick is about to do.
    expect(screen.getByText(messages["releases.pair.pending"]!)).toBeTruthy();
  });

  it("refuses to send both a local time and an exact instant", async () => {
    await mount();
    fireEvent.click(screen.getByRole("button", { name: messages["releases.new"]! }));

    // The hint sits inside the `<label>`, so a field's accessible name is its label *followed by*
    // its hint. Anchored at the start rather than matched exactly, which is what the markup gives.
    const type = (label: string, value: string) => {
      const field = screen.getByLabelText(new RegExp(`^${label}`)) as HTMLInputElement;
      fireEvent.input(field, { target: { value } });
    };
    type(messages["releases.name"]!, "Tết 2028");
    type(messages["releases.wallDate"]!, "2028-02-08");
    type(messages["releases.instant"]!, "2028-02-08T04:00");
    fireEvent.click(screen.getByRole("button", { name: messages["releases.saveDraft"]! }));

    // Said here rather than sent: the server refuses this too, but a round trip to learn that the
    // two fields disagree teaches an operator the console is guessing.
    await waitFor(() =>
      expect(screen.getByText(messages["releases.momentConflict"]!)).toBeTruthy(),
    );
    expect(createRelease).not.toHaveBeenCalled();
  });
});
