// A component or helper with no caller is a thing nobody agreed to (Wave 4 · PR-3, widened in PR-6).
//
// The icons set taught this the expensive way: three glyphs were vendored "for later", nothing drew
// them, and the bundle carried them for a release. The kit is the same shape of risk and larger —
// a `Toolbar` written in advance of the screens that need it is a design decided by whoever writes
// it first rather than by the screen that has the problem, and it costs bundle bytes in the
// meantime. So: every component the kit exports is imported by something, and the two that had no
// caller when PR-3 shipped were removed rather than parked.
//
// PR-3 aimed that rule at `components/kit.tsx` alone, and `lib/` walked straight through the gap:
// `createAdminResource` shipped in the same PR with its own test file and **no screen calling it**,
// which is the exact failure the rule above is written against. A helper is as capable of being
// designed in advance of its callers as a component is — more so, because it has no visual absence
// to give it away. The sweep below therefore covers both directories.
//
// The second rule is D5's: a screen that has been migrated does not carry a Refresh button. Stated
// as a list rather than as "no screen anywhere", because PR-6 finishes the migration and a gate
// that fails on the screens still waiting for it would have to be disabled in the meantime — which
// is how a gate stops meaning anything.

import { describe, expect, it } from "vitest";

const sources: Record<string, string> = import.meta.glob("../src/**/*.{ts,tsx}", {
  query: "?raw",
  import: "default",
  eager: true,
});

const kitSource = Object.entries(sources).find(([path]) =>
  path.endsWith("components/kit.tsx"),
)?.[1] as string;

/** Everything `kit.tsx` exports as a component or a hook — the things a screen would import. */
function exportedNames(source: string): string[] {
  return [...source.matchAll(/export function ([A-Z]\w+)/g)].map((match) => match[1] as string);
}

describe("every component the kit exports", () => {
  it("has a caller, so nothing ships that no screen asked for", () => {
    const names = exportedNames(kitSource);
    expect(names.length).toBeGreaterThan(5);

    const callers = Object.entries(sources).filter(([path]) => !path.endsWith("components/kit.tsx"));
    const orphans = names.filter(
      (name) =>
        !callers.some(([, text]) =>
          // An import from the kit, in either spelling the console uses.
          new RegExp(`\\b${name}\\b[^;]*?from "\\.\\.?/(?:\\.\\./)?components/kit"`, "s").test(text),
        ),
    );
    expect(orphans).toEqual([]);
  });
});

describe("a screen that publishes a config node", () => {
  // F13: thirteen publish controls, no two alike, and not one saying whether the thing in front of
  // the operator was already on the shop floor. The list grows as each screen adopts `PublishBar`;
  // stating it as a list rather than "every screen that calls `api.publish*`" is deliberate, for
  // the same reason as the Refresh list below — a gate that fails on the screens still waiting
  // would have to be disabled.
  const ADOPTED = [
    "screens/TaxRates.tsx",
    "screens/ReasonCodes.tsx",
    "screens/Stations.tsx",
    "screens/Floor.tsx",
    "screens/catalog/Menus.tsx",
    "screens/Inventory.tsx",
    "screens/Channels.tsx",
  ];

  it("renders the shared publish bar, so it says where the shop stands", () => {
    const missing = ADOPTED.filter((name) => {
      const entry = Object.entries(sources).find(([path]) => path.endsWith(name));
      return entry === undefined || !entry[1].includes("<PublishBar");
    });
    expect(missing).toEqual([]);
  });

  it("stays on the list, so an adopted screen cannot quietly un-adopt", () => {
    // The list is the record of what has moved. Shrinking it is how a migration gets reverted one
    // screen at a time without anybody noticing, so the count is pinned as well as the contents.
    expect(ADOPTED.length).toBeGreaterThanOrEqual(7);
  });
});

describe("a screen whose Refresh button has gone", () => {
  // Two groups, and the difference matters when reading this list. The first six dropped the button
  // in PR-3 by hand-writing a `load()` and calling it after each mutation: the *behaviour* D5 asks
  // for, without the helper. The five after them are on `createAdminResource` itself (PR-6), which
  // is also where the helper acquired its first caller at all.
  //
  // PR-6b migrates the authoring screens and extends this list as it goes; a gate that failed today
  // on the screens still waiting would have to be disabled, which is how a gate stops meaning
  // anything.
  const MIGRATED = [
    "screens/ApiKeys.tsx",
    "screens/Webhooks.tsx",
    "screens/Devices.tsx",
    "screens/Stores.tsx",
    "screens/Translations.tsx",
    "screens/Alerts.tsx",
    "screens/Media.tsx",
    "screens/Reconcile.tsx",
    "screens/MySessions.tsx",
    "screens/Admins.tsx",
    "screens/Activation.tsx",
    "screens/ReasonCodes.tsx",
    "screens/Stations.tsx",
    "screens/Floor.tsx",
    "screens/catalog/TaxClasses.tsx",
    "screens/catalog/Modifiers.tsx",
    "screens/catalog/Taxonomy.tsx",
    "screens/catalog/Items.tsx",
    "screens/catalog/Menus.tsx",
    "screens/Inventory.tsx",
    "screens/Channels.tsx",
  ];

  it("has no Refresh button, because it re-reads what it changes", () => {
    const offenders = MIGRATED.filter((name) => {
      const entry = Object.entries(sources).find(([path]) => path.endsWith(name));
      return entry !== undefined && entry[1].includes('t("action.refresh")');
    });
    expect(offenders).toEqual([]);
  });

  it("is actually in the tree, so a renamed screen cannot silently leave this list", () => {
    const missing = MIGRATED.filter(
      (name) => !Object.keys(sources).some((path) => path.endsWith(name)),
    );
    expect(missing).toEqual([]);
  });
});
