// A collapsed nav must never hide the screen you are standing on (Wave 3 · Stage 2).
//
// Six groups and thirty entries were expanded all of the time. Collapsing them by default risks
// the opposite failure — an operator opens the console and the screen they are on is inside a
// closed group — so the default is containment: a group with no remembered answer is open when it
// holds the open screen. What is pinned here is that rule and the three-state distinction it rests
// on, because "the operator closed this" and "this happened to be closed" are different facts and
// a full open/closed snapshot cannot tell them apart.

import { beforeEach, describe, expect, it } from "vitest";

import type { MessageKey } from "../src/i18n";
import { groupOpen, loadRemembered, remember } from "../src/lib/nav-groups";
import { NAV_GROUPS } from "../src/state/screens";

/** By key rather than by index, so reordering the nav does not silently retarget these tests. */
function groupNamed(key: MessageKey) {
  const found = NAV_GROUPS.find((group) => group.key === key);
  if (found === undefined) {
    throw new Error(`no nav group is keyed ${key}`);
  }
  return found;
}

const OVERVIEW = groupNamed("nav.group.overview");
const MASTER_DATA = groupNamed("nav.group.masterData");

beforeEach(() => localStorage.clear());

describe("with nothing remembered", () => {
  it("opens the group holding the open screen", () => {
    expect(
      groupOpen({ items: OVERVIEW.items, remembered: undefined, current: "storeHub" }),
    ).toBe(true);
    expect(
      groupOpen({ items: MASTER_DATA.items, remembered: undefined, current: "storeHub" }),
    ).toBe(false);
  });

  it("needs no special case for the landing screen", () => {
    // The store hub is the tenant-scoped index and lives in Overview, so an operator arriving on a
    // fresh console finds Overview open by the same rule that opens Master data on the catalog.
    expect(OVERVIEW.items).toContain("storeHub");
    expect(groupOpen({ items: MASTER_DATA.items, remembered: undefined, current: "catalog" })).toBe(
      true,
    );
  });

  it("leaves every group closed on a path no screen claims", () => {
    // `/login` and `/setup` render outside the Shell, so this is the defensive case rather than a
    // reachable one — and closed-but-usable is the right answer, not an exception.
    for (const group of NAV_GROUPS) {
      expect(groupOpen({ items: group.items, remembered: undefined, current: undefined })).toBe(
        false,
      );
    }
  });
});

describe("an answer the operator gave", () => {
  it("wins in both directions", () => {
    // Closed even though it holds the open screen…
    expect(groupOpen({ items: OVERVIEW.items, remembered: false, current: "storeHub" })).toBe(false);
    // …and open even though it does not.
    expect(groupOpen({ items: MASTER_DATA.items, remembered: true, current: "storeHub" })).toBe(
      true,
    );
  });

  it("is stored sparsely, so an untouched group keeps following the rule", () => {
    const after = remember({}, MASTER_DATA.key, false);
    expect(after).toEqual({ [MASTER_DATA.key]: false });
    // Overview is absent, not `true` — which is what lets the containment rule still answer for it.
    expect(after[OVERVIEW.key]).toBeUndefined();
    expect(groupOpen({ items: OVERVIEW.items, remembered: after[OVERVIEW.key], current: "storeHub" }))
      .toBe(true);
  });

  it("survives a reload", () => {
    remember({}, MASTER_DATA.key, true);
    expect(loadRemembered()).toEqual({ [MASTER_DATA.key]: true });
  });
});

describe("a corrupt stored value", () => {
  it("degrades to a working nav, never to a nav with nothing open", () => {
    for (const bad of ["not json", '"a string"', "[1,2,3]", "null", '{"nav.group.overview":"yes"}']) {
      localStorage.setItem("pos.dashboard.navGroups", bad);
      const loaded = loadRemembered();
      expect(loaded[OVERVIEW.key]).toBeUndefined();
      // Nothing remembered means the containment rule answers, so the open screen is reachable.
      expect(
        groupOpen({ items: OVERVIEW.items, remembered: loaded[OVERVIEW.key], current: "storeHub" }),
      ).toBe(true);
    }
  });

  it("keeps the boolean entries out of a mixed object", () => {
    localStorage.setItem(
      "pos.dashboard.navGroups",
      JSON.stringify({ [OVERVIEW.key]: false, [MASTER_DATA.key]: "maybe" }),
    );
    expect(loadRemembered()).toEqual({ [OVERVIEW.key]: false });
  });
});
