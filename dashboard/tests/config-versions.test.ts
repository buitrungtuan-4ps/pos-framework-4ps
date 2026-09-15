// A store nobody has published to is not a failed read (Wave 4 · PR-1, V10).
//
// `GET /admin/stores/{id}/config/versions` answers `404` for a store with no tree — correct, and
// what every other "nothing here yet" read in this console answers. The client threw on it, so the
// get-started checklist's publish step showed a red "Could not check" on a store whose only sin was
// being new, which is the first screen a first-time operator sees.
//
// Pinned here rather than in the panel because this is a client-contract fact: `404` on this route
// means an empty list, and anything that asks for the versions gets `[]` without special-casing.

import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { api } from "../src/api/client";

const TENANT = "01M22190WCY5PS7KCA7ET7H679";
const STORE = "01STOREAAAAAAAAAAAAAAAAAAA";

const fetchMock = vi.fn();

beforeEach(() => {
  fetchMock.mockReset();
  vi.stubGlobal("fetch", fetchMock);
});

afterEach(() => {
  vi.unstubAllGlobals();
});

describe("reading a store's config versions", () => {
  it("answers an empty list for a store that has never been published to", async () => {
    fetchMock.mockResolvedValue(new Response(null, { status: 404 }));
    await expect(api.configVersions(TENANT, STORE)).resolves.toEqual([]);
  });

  it("still answers the versions when there are some", async () => {
    const versions = [{ version_id: "01VERSIONAAAAAAAAAAAAAAAAA", at_ms: 1, current: true }];
    fetchMock.mockResolvedValue(
      new Response(JSON.stringify(versions), {
        status: 200,
        headers: { "content-type": "application/json" },
      }),
    );
    await expect(api.configVersions(TENANT, STORE)).resolves.toEqual(versions);
  });

  it("still throws on a real failure, which is not the same as an empty store", async () => {
    // A `403` is the console telling the operator their role cannot read this. Swallowing it into
    // an empty list would draw the checklist as though the work were simply undone.
    fetchMock.mockResolvedValue(new Response("{}", { status: 403 }));
    await expect(api.configVersions(TENANT, STORE)).rejects.toBeTruthy();
  });
});
