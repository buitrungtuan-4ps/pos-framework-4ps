// A kit component with no caller is a component nobody agreed to (Wave 4 · PR-3).
//
// The icons set taught this the expensive way: three glyphs were vendored "for later", nothing drew
// them, and the bundle carried them for a release. The kit is the same shape of risk and larger —
// a `Toolbar` written in advance of the screens that need it is a design decided by whoever writes
// it first rather than by the screen that has the problem, and it costs bundle bytes in the
// meantime. So: every component the kit exports is imported by something, and the two that had no
// caller when PR-3 shipped were removed rather than parked.
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

describe("a screen whose Refresh button has gone", () => {
  // The five the F2 kit already owned, plus Alerts. Each re-reads after its own mutations, and
  // Alerts — the one live screen among them — also revalidates on focus and on a minute's timer.
  // PR-6 migrates the remainder onto `createAdminResource` and extends this list as it goes; a gate
  // that failed today on the screens still waiting would have to be disabled, which is how a gate
  // stops meaning anything.
  const MIGRATED = [
    "screens/ApiKeys.tsx",
    "screens/Webhooks.tsx",
    "screens/Devices.tsx",
    "screens/Stores.tsx",
    "screens/Translations.tsx",
    "screens/Alerts.tsx",
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
