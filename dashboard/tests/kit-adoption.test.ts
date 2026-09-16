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

import en from "../src/i18n/en.json";
import vi from "../src/i18n/vi.json";

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
    "screens/Config.tsx",
    "screens/StoreSettings.tsx",
    "screens/Campaigns.tsx",
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
    expect(ADOPTED.length).toBeGreaterThanOrEqual(10);
  });
});

describe("the Refresh button", () => {
  // D5's rule, and it is no longer a list.
  //
  // Thirty screens carried a Refresh button. Not one was there because an operator wanted it: each
  // screen had hand-written its own signal pair, fetched once on the context gate, and had no idea
  // what to do after a save — so the button was the answer to "the list I am looking at is now
  // wrong", asked thirty times. PR-3 dropped the first six by hand; PR-6 moved eleven more onto
  // `createAdminResource`. The list this rule used to carry was explicitly provisional — "a gate
  // that failed today on the screens still waiting would have to be disabled, which is how a gate
  // stops meaning anything" — and it was waiting on PR-6b, which never came.
  //
  // The last two went together: **Ota**, which hand-rolled a `setInterval` the helper exists to
  // own, and **Reports**, whose button was never a Refresh at all — it applies a date window the
  // operator composes, and now says so. With none left, the rule can be what it always wanted to
  // be: every screen, no exceptions to keep current.
  it("does not exist on any screen, because a screen re-reads what it changes", () => {
    const offenders = Object.entries(sources)
      .filter(([path]) => path.includes("/screens/"))
      .filter(([, source]) => source.includes('t("action.refresh")'))
      .map(([path]) => path.replace(/^.*\/src\//, ""));
    expect(offenders).toEqual([]);
  });

  // The key itself goes when its last caller does. Leaving it in the catalogue is how the button
  // comes back: the next screen that wants one finds a translated string waiting for it.
  it("has no translation left to reach for", () => {
    expect(Object.keys(en)).not.toContain("action.refresh");
    expect(Object.keys(vi)).not.toContain("action.refresh");
  });
});
