// A collapsed nav must never hide the screen you are standing on (Wave 3 · Stage 2, revisited).
//
// Eight groups and thirty entries, one group open at a time. Collapsing them risks the opposite
// failure of the original all-expanded nav — an operator opens the console and the screen they are
// on is inside a closed group — so the default is containment: with nothing remembered, the open
// group is the one holding the open screen. What is pinned here is that rule, the accordion (an
// answer names *one* group, so opening a second closes the first), the marker a closed group wears
// when it does hold the open screen, and the three-state distinction all of it rests on: "the
// operator opened this", "the operator closed the lot" and "the operator has never said" are three
// different facts, and a stored boolean per group cannot tell the last two apart.

import { beforeEach, describe, expect, it } from "vitest";

import type { MessageKey } from "../src/i18n";
import { groupOpen, holdsCurrent, loadOpened, rememberOpened } from "../src/lib/nav-groups";
import { NAV_GROUPS, SCREENS, type ScreenId } from "../src/state/screens";

/** By key rather than by index, so reordering the nav does not silently retarget these tests. */
function groupNamed(key: MessageKey) {
  const found = NAV_GROUPS.find((group) => group.key === key);
  if (found === undefined) {
    throw new Error(`no nav group is keyed ${key}`);
  }
  return found;
}

const OVERVIEW = groupNamed("nav.group.overview");
const MENU = groupNamed("nav.group.menu");

beforeEach(() => localStorage.clear());

describe("the grouping itself", () => {
  it("files every nav-reachable screen exactly once", () => {
    // Two groups were dumping grounds and screens are being moved between them, which is exactly
    // when a screen gets dropped or filed twice — neither of which any other check would see.
    const filed = NAV_GROUPS.flatMap((group) => group.items);
    expect([...new Set(filed)].sort()).toEqual([...filed].sort());
    // `newStore` is deliberately out: the wizard is reached through Stores, not from the sidebar
    // (scripts/step-budget.mjs pins that flow). Every other screen must be reachable from the nav.
    const unfiled = (Object.keys(SCREENS) as ScreenId[]).filter((id) => !filed.includes(id));
    expect(unfiled).toEqual(["newStore"]);
  });

  it("keeps a group readable at a glance", () => {
    // Six is the ceiling. "Master data" held eleven, which is the length that made an operator read
    // all thirty names to find one — the complaint this regrouping answers.
    for (const group of NAV_GROUPS) {
      expect(group.items.length).toBeLessThanOrEqual(6);
      expect(group.items.length).toBeGreaterThan(0);
    }
  });
});

describe("with nothing remembered", () => {
  it("opens the group holding the open screen", () => {
    expect(
      groupOpen({ key: OVERVIEW.key, items: OVERVIEW.items, opened: undefined, current: "storeHub" }),
    ).toBe(true);
    expect(
      groupOpen({ key: MENU.key, items: MENU.items, opened: undefined, current: "storeHub" }),
    ).toBe(false);
  });

  it("needs no special case for the landing screen", () => {
    // The store hub is the tenant-scoped index and lives in Overview, so an operator arriving on a
    // fresh console finds Overview open by the same rule that opens Menu & pricing on the catalog.
    expect(OVERVIEW.items).toContain("storeHub");
    expect(
      groupOpen({ key: MENU.key, items: MENU.items, opened: undefined, current: "catalog" }),
    ).toBe(true);
  });

  it("leaves every group closed on a screen no group files", () => {
    // The reachable instance is the new-store wizard, which is in `SCREENS` and in no group. All
    // eight headings closed is the honest answer there — no entry corresponds to the wizard, so
    // marking one would be a lie — and eight short headings is a usable nav, not a broken one.
    for (const current of ["newStore", undefined] as const) {
      for (const group of NAV_GROUPS) {
        expect(
          groupOpen({ key: group.key, items: group.items, opened: undefined, current }),
        ).toBe(false);
      }
    }
  });
});

describe("an answer the operator gave", () => {
  it("wins in both directions", () => {
    // Closed even though it holds the open screen…
    expect(
      groupOpen({ key: OVERVIEW.key, items: OVERVIEW.items, opened: MENU.key, current: "storeHub" }),
    ).toBe(false);
    // …and open even though it does not.
    expect(
      groupOpen({ key: MENU.key, items: MENU.items, opened: MENU.key, current: "storeHub" }),
    ).toBe(true);
  });

  it("names one group, which is what makes it an accordion", () => {
    // The answer is a single key rather than a map, so there is no state in which two groups are
    // open — the nav's height cannot grow with the number of groups the console gains.
    const opened = MENU.key;
    const open = NAV_GROUPS.filter((group) =>
      groupOpen({ key: group.key, items: group.items, opened, current: "storeHub" }),
    );
    expect(open.map((group) => group.key)).toEqual([MENU.key]);
  });

  it("can be 'none of them', which the containment rule must not undo", () => {
    // Closing the open group is a reachable state and a legitimate one: the headings alone are the
    // whole index. It has to survive standing on a screen, or the group would spring back open.
    for (const group of NAV_GROUPS) {
      expect(
        groupOpen({ key: group.key, items: group.items, opened: null, current: "storeHub" }),
      ).toBe(false);
    }
  });

  it("survives a reload, in both of its forms", () => {
    rememberOpened(MENU.key);
    expect(loadOpened()).toBe(MENU.key);
    rememberOpened(null);
    expect(loadOpened()).toBeNull();
  });
});

describe("a closed group holding the open screen", () => {
  it("is the one thing the heading has to say", () => {
    // Its entries are `hidden`, so the entry that would have said "you are here" cannot. Without
    // the heading saying it, the accordion can put an operator somewhere the nav does not admit to.
    expect(holdsCurrent(MENU.items, "catalog")).toBe(true);
    expect(holdsCurrent(MENU.items, "storeHub")).toBe(false);
    expect(holdsCurrent(MENU.items, undefined)).toBe(false);
  });
});

describe("a corrupt stored value", () => {
  it("degrades to a working nav, never to a nav with nothing open", () => {
    for (const bad of [
      "not json",
      '"a string"',
      "[1,2,3]",
      "null",
      "{}",
      '{"opened":7}',
      // The shape the key held before the accordion: a map of every group the operator had toggled.
      // An upgrade meets this, so it has to read as "nothing remembered" rather than as an answer.
      '{"nav.group.overview":false,"nav.group.masterData":true}',
      // A group that no longer exists. Taken at face value this is the failure mode that matters:
      // no heading matches, so nothing is open and the open screen is unreachable from the nav.
      '{"opened":"nav.group.masterData"}',
    ]) {
      localStorage.setItem("pos.dashboard.navGroups", bad);
      const opened = loadOpened();
      expect(opened).toBeUndefined();
      // Nothing remembered means the containment rule answers, so the open screen is reachable.
      expect(
        groupOpen({ key: OVERVIEW.key, items: OVERVIEW.items, opened, current: "storeHub" }),
      ).toBe(true);
    }
  });

  it("tells an explicit 'none of them' apart from a value it could not read", () => {
    localStorage.setItem("pos.dashboard.navGroups", JSON.stringify({ opened: null }));
    expect(loadOpened()).toBeNull();
    localStorage.setItem("pos.dashboard.navGroups", JSON.stringify({ opened: "nope" }));
    expect(loadOpened()).toBeUndefined();
  });
});
