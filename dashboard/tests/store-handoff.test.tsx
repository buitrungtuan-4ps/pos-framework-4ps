// Getting a store's files back, from the Stores screen.
//
// The gap this closes was not a bug in anything — every piece worked. The wizard rendered four
// files for a store it had just created, and no screen rendered them for a store that already
// existed, so replacing a dead machine meant assembling a `config.toml` by hand from a generator's
// source. What is pinned here is the wiring, because the wiring is where it can silently come apart:
//
//   * the affordance is on the row, so it is reached from the store rather than from a wizard that
//     would create a *second* store;
//   * opening it makes no write. A drawer an operator opens to read what a dead machine costs must
//     not mint a credential on the way in;
//   * the key it issues is scoped to that store and carries the two scopes `/sync` needs — a
//     tenant-wide key is refused by the store sync routes, so issuing one would hand over a
//     credential the box cannot use, and nothing downstream would say why;
//   * the downloads stay closed until the key question is settled, in *either* direction. The
//     silent version of "no key" is an `env` file with no credential in it, which produces a store
//     that trades and never syncs.

import { cleanup, fireEvent, render, screen, waitFor } from "@solidjs/testing-library";
import { MemoryRouter, Route } from "@solidjs/router";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { Stores } from "../src/screens/Stores";
import { selectTenant } from "../src/state/session";
import en from "../src/i18n/en.json";

const TENANT = { tenant_id: "01M22190WCY5PS7KCA7ET7H679", name: "Pizza 4P's Vietnam" };
const STORE = {
  store_id: "01M221BB8BB5SESQDB895SJHJS",
  name: "4P's Bến Thành",
  brand_id: null,
  status: "active",
  etag: "v1",
};

const createApiKey = vi.fn();
const getOtaRollout = vi.fn();
const listHostedRelease = vi.fn();

vi.mock("../src/api/client", () => ({
  api: {
    listStores: () => Promise.resolve([STORE]),
    listBrands: () => Promise.resolve([]),
    listTenants: () => Promise.resolve([{ ...TENANT, status: "active", etag: "v1" }]),
    createApiKey: (...args: unknown[]) => createApiKey(...args),
    getOtaRollout: (...args: unknown[]) => getOtaRollout(...args),
    listHostedRelease: (...args: unknown[]) => listHostedRelease(...args),
    createStore: vi.fn(),
    createBrand: vi.fn(),
    updateStore: vi.fn(),
    updateBrand: vi.fn(),
    updateTenant: vi.fn(),
  },
  ApiError: class ApiError extends Error {},
}));

/**
 * The key the mocked route hands back.
 *
 * Deliberately **not** shaped like a vendor credential. The first push used a Stripe-style
 * live-key prefix and the `secrets` job refused it under gitleaks' `stripe-access-token` rule,
 * correctly: a fixture wearing that prefix is indistinguishable from a real leak to every scanner
 * that will ever read this repository, and the scanner that matters is the one guarding an actual
 * key. The prefix is described here rather than written out, so this comment cannot trip the same
 * rule the value did.
 */
const TOKEN = "test-store-key-not-a-credential";

const messages = en as Record<string, string>;

function mountStores() {
  return render(() => (
    <MemoryRouter>
      <Route path="/" component={Stores} />
    </MemoryRouter>
  ));
}

/**
 * Opens the drawer for the one store in the table.
 *
 * Two clicks since Wave 4 · PR-3: the row's three verbs moved behind a kebab, so the handoff is
 * reached through it rather than from a button sitting in the row. The extra step is asserted
 * rather than worked around — "is reachable from the store's own row" is the claim this file makes,
 * and a helper that reached past the menu would stop testing it.
 */
async function openHandoff() {
  mountStores();
  await waitFor(() => expect(screen.getByText(STORE.name)).toBeTruthy());
  fireEvent.click(screen.getAllByRole("button", { name: messages["common.actions"] })[0] as Element);
  fireEvent.click(screen.getByRole("button", { name: messages["handoff.open"] }));
  await waitFor(() => expect(screen.getByRole("dialog")).toBeTruthy());
}

describe("handing a store's files over again", () => {
  beforeEach(() => {
    localStorage.clear();
    vi.clearAllMocks();
    createApiKey.mockResolvedValue({ id: "01M221KEY", token: TOKEN });
    getOtaRollout.mockResolvedValue(null);
    listHostedRelease.mockResolvedValue({ release: "", artifacts: [] });
    selectTenant(TENANT.tenant_id, TENANT.name);
  });
  afterEach(cleanup);

  it("is reachable from the store's own row", async () => {
    await openHandoff();
    // Named for the store it is about, so an operator with an estate open in two tabs can tell
    // which shop's credential they are about to mint.
    expect(screen.getByRole("dialog").textContent).toContain(STORE.name);
  });

  it("issues nothing until asked", async () => {
    await openHandoff();
    expect(createApiKey).not.toHaveBeenCalled();
    // And the files are not offered yet either: the key question is a fork in the road, not a
    // default. `queryAllByRole` because the download buttons are what must be absent.
    expect(
      screen.queryByRole("button", { name: messages["handoff.downloadWindows"] }),
    ).toBeNull();
  });

  it("scopes the key it issues to this store, with the two scopes /sync needs", async () => {
    await openHandoff();
    fireEvent.click(screen.getByRole("button", { name: messages["handoff.issueKey"] }));
    await waitFor(() => expect(createApiKey).toHaveBeenCalledTimes(1));
    expect(createApiKey).toHaveBeenCalledWith(
      TENANT.tenant_id,
      ["read_config", "relay_orders"],
      STORE.store_id,
    );
    // Shown once, so it has to be on screen now — it is baked into the four files below and the
    // cloud stores only its hash.
    await waitFor(() =>
      expect(screen.getByRole("dialog").textContent).toContain(TOKEN),
    );
  });

  it("opens the four downloads once the key question is settled", async () => {
    await openHandoff();
    fireEvent.click(screen.getByRole("button", { name: messages["handoff.issueKey"] }));
    await waitFor(() =>
      expect(screen.getByRole("button", { name: messages["handoff.downloadWindows"] })).toBeTruthy(),
    );
    for (const key of [
      "handoff.downloadWindows",
      "handoff.downloadLinux",
      "handoff.downloadConfig",
      "handoff.downloadEnv",
    ]) {
      expect(screen.getByRole("button", { name: messages[key] })).toBeTruthy();
    }
  });

  it("lets a box that already holds its key through, without a write", async () => {
    // The other direction out of the fork. It must reach the files — a box being repaired by hand
    // is a real case — and it must say plainly that they carry no credential, because the download
    // itself looks identical either way.
    await openHandoff();
    fireEvent.click(screen.getByRole("button", { name: messages["wizard.skipKey"] }));
    await waitFor(() =>
      expect(screen.getByRole("button", { name: messages["handoff.downloadEnv"] })).toBeTruthy(),
    );
    expect(createApiKey).not.toHaveBeenCalled();
    expect(screen.getByRole("dialog").textContent).toContain(messages["handoff.withoutKey"]);
  });

  it("offers the Windows setup file for the release the store's rollout targets", async () => {
    // The one-file path (ADR-0141): the release's own executable, under a name that installs this
    // store and dials the cloud this browser is looking at. The version starts at the rollout's
    // target, and the link appears only once the cloud says it holds a Windows build of it.
    getOtaRollout.mockResolvedValue({
      target_version: "1.5.0",
      min_ring: "fleet",
      rollout_percent: 100,
      signing_key_id: "test-key",
    });
    listHostedRelease.mockResolvedValue({
      release: "1.5.0",
      artifacts: [
        { arch: "x86_64-pc-windows-msvc", size_bytes: 1, sha256: "00", recorded_at_ms: 0 },
      ],
    });
    await openHandoff();
    fireEvent.click(screen.getByRole("button", { name: messages["wizard.skipKey"] }));
    const link = await waitFor(() =>
      screen.getByRole("link", { name: messages["setupFile.download"] }),
    );
    const query = new URLSearchParams({ store_id: STORE.store_id, cloud: window.location.host });
    expect(link.getAttribute("href")).toBe(
      `/admin/ota/releases/1.5.0/installer?${query.toString()}`,
    );
    expect(listHostedRelease).toHaveBeenCalledWith("1.5.0");
    // No key was issued, so the setup window's question is answered in advance: skip it, and what
    // that costs.
    expect(screen.getByRole("dialog").textContent).toContain(messages["setupFile.noKey"]);
  });

  it("says so when the cloud holds no Windows build of the release", async () => {
    getOtaRollout.mockResolvedValue({
      target_version: "1.5.0",
      min_ring: "fleet",
      rollout_percent: 100,
      signing_key_id: "test-key",
    });
    listHostedRelease.mockResolvedValue({
      release: "1.5.0",
      artifacts: [
        { arch: "x86_64-unknown-linux-gnu", size_bytes: 1, sha256: "00", recorded_at_ms: 0 },
      ],
    });
    await openHandoff();
    fireEvent.click(screen.getByRole("button", { name: messages["wizard.skipKey"] }));
    const expected = (messages["setupFile.notHosted"] ?? "").replace("{release}", "1.5.0");
    await waitFor(() => expect(screen.getByRole("dialog").textContent).toContain(expected));
    expect(screen.queryByRole("link", { name: messages["setupFile.download"] })).toBeNull();
  });

  it("says the device credential is gone with the machine", async () => {
    // The whole reason the drawer leads with prose. Everything else about a replacement handoff
    // looks finished without this step, and a box that installed cleanly and will not sell is the
    // most expensive way to learn it.
    await openHandoff();
    const body = screen.getByRole("dialog").textContent ?? "";
    expect(body).toContain(messages["handoff.factCredential"]);
    expect(body).toContain(messages["handoff.recoveryLost"]);
  });
});
