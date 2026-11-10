// A draft is the publish's body without the tenant, and a part shows the refusals that name it.

import { beforeEach, describe, expect, it } from "vitest";

import type { PackageRefusal, PackageVersionRecord } from "../src/api/schemas";
import {
  blankDraft,
  draftFromVersion,
  draftRequest,
  dropDraft,
  loadDraft,
  refusalsFor,
  saveDraft,
} from "../src/lib/package-draft";

const VERSION = {
  content: {
    package_id: "01J00000000000000000000P01",
    package_version_id: "01J00000000000000000000V01",
    version_label: "March",
    min_edge_version: "0.14.1",
    time_slots: [],
    flows: [],
    reactions: [],
    data: {},
    screens: [{ selector: {}, body: { screen_id: "welcome", root: { component_id: "stack" } } }],
  },
  create_time: "2026-11-10T00:00:00Z",
} as unknown as PackageVersionRecord;

function refusal(field: string): PackageRefusal {
  return { field, reason: "INVALID", message: "" };
}

describe("a package draft", () => {
  beforeEach(() => localStorage.clear());

  it("opens a version with every part copied, and sends it with the tenant and a trimmed label", () => {
    const draft = draftFromVersion(VERSION);
    expect(draft.screens).toEqual(VERSION.content.screens);
    expect(draftRequest("01TENANT", { ...draft, version_label: " April " })).toMatchObject({
      tenant_id: "01TENANT",
      version_label: "April",
      screens: VERSION.content.screens,
    });
    expect(blankDraft()).toEqual({ version_label: "", time_slots: [], data: {}, screens: [] });
  });

  it("gives a part the refusals of its field and the fields below it, and no other", () => {
    const all = [refusal("screens"), refusal("data.object_types"), refusal("version_label"), refusal("database")];
    expect(refusalsFor(all, "screens").map((r) => r.field)).toEqual(["screens"]);
    expect(refusalsFor(all, "data").map((r) => r.field)).toEqual(["data.object_types"]);
  });

  it("is kept per tenant and package until it is dropped", () => {
    saveDraft("01TENANT", "01PACKAGE", blankDraft("May"));
    expect(loadDraft("01TENANT", "01PACKAGE")?.version_label).toBe("May");
    expect(loadDraft("01OTHER", "01PACKAGE")).toBeNull();
    dropDraft("01TENANT", "01PACKAGE");
    expect(loadDraft("01TENANT", "01PACKAGE")).toBeNull();
  });
});
