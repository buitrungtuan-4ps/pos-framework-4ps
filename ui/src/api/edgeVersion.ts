// Which release the edge is running, and whether this app is ahead of it (ADR-0111).
//
// # Why a response header and not a read
//
// Drift shows up *after* pairing — an OTA ring moves the edge on a Tuesday, or a shell updates
// itself overnight — so a version read once at pairing time is a version that was true once. Every
// `/api/*` answer carries `pos-edge-version`, so the app learns which release replied on the call it
// just made, including the call that just failed.
//
// # The comparison is one-sided, and a snapshot is what makes that safe
//
// Only "the app is ahead of its edge" is checked. The other direction needs no check because a newer
// edge cannot take a route away from an older app: `docs/snapshots/routes.txt` fails the build if a
// published `/api/*` route is renamed or removed (ADR-0111, AGENTS.md §2). Without that this
// comparison would rest on a promise nothing keeps.
//
// # What "behind" costs
//
// A banner, and nothing else. ADR-0024 settled the principle one tier down — "a protocol mismatch
// degrades to 'not syncing', never to 'not selling'" — and a version string is not a reason to
// refuse a customer.

import { createSignal } from "solid-js";

// The oldest edge release this app knows it works against. The release workflow injects
// `VITE_MINIMUM_EDGE_VERSION` from the tag it is building, so the bundle embedded in an edge always
// matches the binary serving it and never warns. A local build has none and reads `0.0.0`, which
// sorts below every release and therefore never warns either — the same honesty
// `crates/pos-edge/src/version.rs` applies to a hand-built binary.
const env = import.meta.env as Record<string, string | undefined>;
export const MINIMUM_EDGE_VERSION: string = env.VITE_MINIMUM_EDGE_VERSION ?? "0.0.0";

// The header the edge stamps on every `/api/*` response.
const HEADER = "pos-edge-version";

const [edgeVersion, setEdgeVersion] = createSignal<string | null>(null);
export { edgeVersion };

// `X.Y.Z` as three numbers, or `null` for anything else. Deliberately strict: a value this cannot
// parse is a value it must not rank, because ranking it wrongly shows a banner naming a version
// nobody recognises.
function parse(version: string): [number, number, number] | null {
  const parts = version.split(".");
  if (parts.length !== 3) {
    return null;
  }
  const numbers = parts.map((part) => (/^\d+$/.test(part) ? Number(part) : Number.NaN));
  const [major, minor, patch] = numbers as [number, number, number];
  if (Number.isNaN(major) || Number.isNaN(minor) || Number.isNaN(patch)) {
    return null;
  }
  return [major, minor, patch];
}

// Negative when `left` is older than `right`, zero when equal, positive when newer.
function compare(left: [number, number, number], right: [number, number, number]): number {
  for (let index = 0; index < 3; index += 1) {
    const difference = (left[index] ?? 0) - (right[index] ?? 0);
    if (difference !== 0) {
      return difference;
    }
  }
  return 0;
}

// Records the release that answered, if the response named one. Called on every `fetch` the client
// makes — including `signIn` and `signOut`, which bypass `request()` on purpose.
export function observeEdgeVersion(response: Response): void {
  const reported = response.headers.get(HEADER);
  if (reported !== null && reported !== "") {
    setEdgeVersion(reported);
  }
}

// Whether the edge that answered is older than the release this app was built for.
//
// False until a response has been seen, false for an edge whose version does not parse, and false
// for `0.0.0` on either side: a developer build is not a release, and a banner on every call of
// `just run-edge` is noise that teaches an operator to ignore banners.
export function edgeIsBehind(): boolean {
  const reported = edgeVersion();
  if (reported === null) {
    return false;
  }
  const running = parse(reported);
  const required = parse(MINIMUM_EDGE_VERSION);
  if (running === null || required === null) {
    return false;
  }
  const unstamped: [number, number, number] = [0, 0, 0];
  if (compare(running, unstamped) === 0 || compare(required, unstamped) === 0) {
    return false;
  }
  return compare(running, required) < 0;
}
