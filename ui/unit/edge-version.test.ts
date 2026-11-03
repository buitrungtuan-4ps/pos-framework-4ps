// `api/edgeVersion.ts` (ADR-0111): the app warns only when a release edge is older than the release
// it was built for, and stays quiet for every case it cannot rank.

import { afterEach, describe, expect, it, vi } from "vitest";

/** A fresh copy of the module, built for `minimum`, that has seen a response naming `reported`. */
async function seen(minimum: string | undefined, reported: string | null) {
  vi.resetModules();
  vi.stubEnv("VITE_MINIMUM_EDGE_VERSION", minimum);
  const module = await import("../src/api/edgeVersion");
  if (reported !== null) {
    module.observeEdgeVersion(new Response(null, { headers: { "pos-edge-version": reported } }));
  }
  return module;
}

afterEach(() => {
  vi.unstubAllEnvs();
});

describe("edgeIsBehind", () => {
  it("is true for a release edge older than the release the app was built for", async () => {
    expect((await seen("1.4.0", "1.3.9")).edgeIsBehind()).toBe(true);
    expect((await seen("1.4.0", "0.9.12")).edgeIsBehind()).toBe(true);
  });

  it("is false for the same release or a newer one", async () => {
    expect((await seen("1.4.0", "1.4.0")).edgeIsBehind()).toBe(false);
    expect((await seen("1.4.0", "1.10.0")).edgeIsBehind()).toBe(false);
  });

  it("is false until a response has named a version", async () => {
    const module = await seen("1.4.0", null);
    expect(module.edgeVersion()).toBeNull();
    expect(module.edgeIsBehind()).toBe(false);
  });

  it("is false for a version it cannot parse", async () => {
    for (const reported of ["1.3", "1.3.0-rc.1", "v1.3.0", "1.3.x", "1.3.0.1"]) {
      expect((await seen("1.4.0", reported)).edgeIsBehind()).toBe(false);
    }
  });

  it("is false for 0.0.0 on either side, which is a developer build", async () => {
    expect((await seen("1.4.0", "0.0.0")).edgeIsBehind()).toBe(false);
    expect((await seen("0.0.0", "0.0.1")).edgeIsBehind()).toBe(false);
    expect((await seen(undefined, "0.0.1")).edgeIsBehind()).toBe(false);
  });
});

describe("observeEdgeVersion", () => {
  it("keeps the last version named and ignores a response that names none", async () => {
    const module = await seen("1.4.0", "1.3.0");
    module.observeEdgeVersion(new Response(null));
    module.observeEdgeVersion(new Response(null, { headers: { "pos-edge-version": "" } }));
    expect(module.edgeVersion()).toBe("1.3.0");
  });
});
