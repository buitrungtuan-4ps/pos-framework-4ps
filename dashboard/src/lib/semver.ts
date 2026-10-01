// Whether a store's release honours a setting
// ([ADR-0160](../../../docs/adr/0160-everything-a-store-runs-differently-is-published-configuration.md)
// decision 5, "honour or hide").
//
// Every setting in the register names the first release that honours it (`since`), and the fleet
// read names the release each store last reported (`installed_version`). A store on an older
// release ignores the value, so the settings screen hides the setting for that store rather than
// offering a switch that does nothing there.
//
// # The comparison
//
// Numeric, component by component — major, then minor, then patch — which is how `pos-core`'s
// `ReleaseVersion` orders the same strings. A string comparison would put `0.9.0` after `0.14.1`.
//
// # Unknown is not older
//
// A store that never reported, or reported something that is not `MAJOR.MINOR.PATCH`, is
// **unknown**, and the screen shows the setting with a note rather than hiding it. Hiding on a
// guess would take a working switch away from a store that may well honour it, and a store that
// has not reported yet is exactly the new store an operator is setting up. The one tolerance is a
// leading `v`: the release tags are `v1.2.0` and the edge strips the `v` before it reports, but an
// older report or a hand-entered one may carry it, and it means the same release.

/** A release, as its three numbers. */
export interface Release {
  readonly major: number;
  readonly minor: number;
  readonly patch: number;
}

const RELEASE = /^v?(\d+)\.(\d+)\.(\d+)$/;

/** The release `text` names, or `null` when it is absent or not `MAJOR.MINOR.PATCH`. */
export function parseRelease(text: string | null | undefined): Release | null {
  if (typeof text !== "string") {
    return null;
  }
  const match = RELEASE.exec(text.trim());
  if (match === null) {
    return null;
  }
  const [major, minor, patch] = [match[1], match[2], match[3]].map(Number);
  // A component past 2^53 does not survive `Number`, and a release nobody can compare is unknown.
  if (
    major === undefined ||
    minor === undefined ||
    patch === undefined ||
    ![major, minor, patch].every(Number.isSafeInteger)
  ) {
    return null;
  }
  return { major, minor, patch };
}

/**
 * `-1` when `left` is the older release, `1` when it is the newer, `0` when they are the same, and
 * `null` when either cannot be read — which is never "older".
 */
export function compareReleases(
  left: string | null | undefined,
  right: string | null | undefined,
): -1 | 0 | 1 | null {
  const a = parseRelease(left);
  const b = parseRelease(right);
  if (a === null || b === null) {
    return null;
  }
  for (const [x, y] of [
    [a.major, b.major],
    [a.minor, b.minor],
    [a.patch, b.patch],
  ] as const) {
    if (x !== y) {
      return x < y ? -1 : 1;
    }
  }
  return 0;
}

/** Whether a store on `installed` honours a setting honoured from `since`. */
export type ReleaseStanding = "honours" | "older" | "unknown";

/** See {@link ReleaseStanding}: `unknown` when either release cannot be read, never `older`. */
export function releaseStanding(
  installed: string | null | undefined,
  since: string,
): ReleaseStanding {
  const order = compareReleases(installed, since);
  if (order === null) {
    return "unknown";
  }
  return order < 0 ? "older" : "honours";
}
