// The new-store wizard hands out no store file from a console on plain http (pilot F1).
//
// Every file on the wizard's last step — config.toml, the env file, both installers — names the
// origin this browser is looking at as the store's cloud, and the edge dials its cloud over https
// only, loopback included. The pilot ran the console over http, took the files, and got a box that
// logged one error and traded LAN-only with an empty roster and menu. Only the Windows setup file
// said why. Pinned here: over http none of the files is offered and the step says why; over https
// all of them are, so the gate cannot quietly close for everyone.

import { MemoryRouter, Route } from "@solidjs/router";
import { cleanup, fireEvent, render, screen, waitFor } from "@solidjs/testing-library";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { NewStore } from "../src/screens/NewStore";
import { selectTenant } from "../src/state/session";
import en from "../src/i18n/en.json";

const TENANT = { id: "01M22190WCY5PS7KCA7ET7H679", name: "Pizza 4P's Vietnam" };
const CREATED = {
  store_id: "01M2219QK4T3W6Z0Y8FBQ2X5MV",
  tenant_id: TENANT.id,
  brand_id: null,
  name: "Xuân Thủy",
  status: "active",
  etag: "s1",
};

vi.mock("../src/api/client", () => ({
  api: {
    listBrands: () => Promise.resolve([]),
    createStore: () => Promise.resolve(CREATED),
    applySettingPresets: () => Promise.resolve({ applied: [], stores: [] }),
    getOtaRollout: () => Promise.resolve(null),
    listHostedRelease: () => Promise.resolve({ release: "", artifacts: [] }),
  },
  ApiError: class ApiError extends Error {},
}));

const messages = en as Record<string, string>;

/** Vitest's jsdom instance, which is what lets a case put the console at an `https` origin. */
const page = (globalThis as unknown as { jsdom: { reconfigure(options: { url: string }): void } })
  .jsdom;

/** The four downloads the last step offers. */
const DOWNLOADS = [
  "wizard.downloadConfig",
  "wizard.downloadEnv",
  "wizard.downloadInstaller",
  "wizard.downloadWindowsInstaller",
];

/** Creates the store, skips the key, and lands on the handoff step. */
async function reachHandoff() {
  render(() => (
    <MemoryRouter>
      <Route path="/" component={NewStore} />
    </MemoryRouter>
  ));
  fireEvent.input(screen.getByLabelText(/^Name/), { target: { value: CREATED.name } });
  fireEvent.click(screen.getByRole("button", { name: messages["wizard.next"] }));
  fireEvent.click(await screen.findByRole("button", { name: messages["wizard.skipKey"] }));
  await waitFor(() => expect(screen.getByText(messages["wizard.doneTitle"] ?? "")).toBeTruthy());
}

describe("the wizard's handoff step", () => {
  beforeEach(() => {
    localStorage.clear();
    selectTenant(TENANT.id, TENANT.name);
  });
  afterEach(() => {
    cleanup();
    page.reconfigure({ url: "http://localhost:3000/" });
  });

  it("offers no store file to a console on plain http, and says why", async () => {
    await reachHandoff();
    expect(screen.getByText(messages["handoff.needsHttps"] ?? "")).toBeTruthy();
    for (const key of DOWNLOADS) {
      expect(screen.queryByRole("button", { name: messages[key] })).toBeNull();
    }
    // Nor the config's text, which an operator could copy by hand instead.
    expect(document.body.textContent).not.toContain("cloud_url");
  });

  it("offers every file, naming the https origin, to a console on https", async () => {
    page.reconfigure({ url: "https://console.example.com/" });
    await reachHandoff();
    for (const key of DOWNLOADS) {
      expect(screen.getByRole("button", { name: messages[key] })).toBeTruthy();
    }
    expect(document.body.textContent).toContain('cloud_url = "https://console.example.com"');
    expect(screen.queryByText(messages["handoff.needsHttps"] ?? "")).toBeNull();
  });
});
