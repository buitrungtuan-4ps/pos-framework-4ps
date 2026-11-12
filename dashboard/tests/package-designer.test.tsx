// The package designer's shell (ADR-0190 decision 8): a package's newest version opens as a draft,
// the cloud's dry run is asked before a publish, a refusal is shown beside what it refuses, and only
// a draft the cloud accepts is published.

import { MemoryRouter, Route } from "@solidjs/router";
import { cleanup, fireEvent, render, screen, waitFor } from "@solidjs/testing-library";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import en from "../src/i18n/en.json";
import { PackageDesigner } from "../src/screens/PackageDesigner";
import { selectTenant } from "../src/state/session";

const messages = en as Record<string, string>;
const TENANT = "01M22190WCY5PS7KCA7ET7H679";
const PACKAGE = { package_id: "01J00000000000000000000P01", name: "Counter screens", create_time: "2026-11-10T00:00:00Z" };
const VERSION = {
  content: {
    package_id: PACKAGE.package_id,
    package_version_id: "01J00000000000000000000V01",
    version_label: "March",
    min_edge_version: "0.14.1",
    time_slots: [],
    flows: [],
    reactions: [],
    data: {},
    screens: [],
  },
  create_time: "2026-11-10T00:00:00Z",
};

const dryRun = vi.fn();
const publish = vi.fn();

vi.mock("../src/api/packages", () => ({
  listPackages: () => Promise.resolve([PACKAGE]),
  listPackageVersions: () => Promise.resolve([VERSION]),
  createPackage: vi.fn(),
  dryRunPackageVersion: (...args: unknown[]) => dryRun(...args),
  publishPackageVersion: (...args: unknown[]) => publish(...args),
}));

async function mount() {
  render(() => (
    <MemoryRouter>
      <Route path="/" component={PackageDesigner} />
    </MemoryRouter>
  ));
  await waitFor(() => expect(screen.getByRole("option", { name: PACKAGE.name })).toBeTruthy());
  fireEvent.change(screen.getByLabelText(messages["designer.package"]!), { target: { value: PACKAGE.package_id } });
  await waitFor(() => expect(screen.getByLabelText(messages["designer.versionLabel"]!)).toBeTruthy());
}

/** The draft this browser keeps for the package. */
function kept(): string | null {
  return localStorage.getItem(`pos.package-draft.${TENANT}.${PACKAGE.package_id}`);
}

function label(): HTMLInputElement {
  return screen.getByLabelText(messages["designer.versionLabel"]!) as HTMLInputElement;
}

describe("the package designer", () => {
  beforeEach(() => {
    localStorage.clear();
    vi.clearAllMocks();
    selectTenant(TENANT, "Pizza 4P's Vietnam");
  });
  afterEach(cleanup);

  it("shows a refusal under the label it refuses, and does not publish", async () => {
    dryRun.mockResolvedValue({
      accepted: false,
      min_edge_version: "0.14.1",
      refusals: [{ field: "version_label", reason: "INVALID", message: "a version label is 1 to 64 characters" }],
    });
    await mount();
    expect(label().value).toBe("March");
    fireEvent.input(label(), { target: { value: " " } });
    fireEvent.click(screen.getByRole("button", { name: messages["designer.publish"]! }));
    await waitFor(() => expect(screen.getByText(messages["designer.refusal.versionLabel"]!)).toBeTruthy());
    expect(dryRun).toHaveBeenCalledWith(PACKAGE.package_id, expect.objectContaining({ tenant_id: TENANT, version_label: "" }));
    expect(publish).not.toHaveBeenCalled();
  });

  it("publishes a draft the cloud accepts, and forgets the kept draft", async () => {
    dryRun.mockResolvedValue({ accepted: true, min_edge_version: "0.14.1", refusals: [], warnings: [] });
    publish.mockResolvedValue({ ...VERSION, content: { ...VERSION.content, version_label: "April" } });
    await mount();
    fireEvent.input(label(), { target: { value: "April" } });
    expect(kept()).not.toBeNull();
    fireEvent.click(screen.getByRole("button", { name: messages["designer.publish"]! }));
    await waitFor(() => expect(publish).toHaveBeenCalledWith(PACKAGE.package_id, expect.objectContaining({ version_label: "April" })));
    await waitFor(() => expect(document.querySelector('[data-outcome="package-version-published"]')).toBeTruthy());
    expect(kept()).toBeNull();
  });
});
