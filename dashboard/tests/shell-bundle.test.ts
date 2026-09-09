// What the first visit downloads (Wave 3 · Stage 6).
//
// # The regression this was written from
//
// The account menu added in this stage wanted one status pill, and imported `StatusBadge` from
// `components/kit.tsx`. That is a correct import and it typechecks, and it moved the entire CRUD kit
// — every screen's table, modal, drawer, pager and confirm dialog — out of its lazy chunk and into
// the bundle every first visit downloads: 50.6 kB to 64.9 kB, for a coloured `<span>`.
//
// Nothing would have caught it. `tsc` is happy, every test passed, the screens all worked. The only
// evidence was the `kit-*.js` chunk disappearing from a build log nobody diffs. The fix was to move
// `StatusBadge` to `components/ui.tsx`, where a dependency-free primitive belongs — but the fix is
// not the point, because the next component to want one thing from the kit will do this again.
//
// # What this checks
//
// The eager module graph: everything reachable from `main.tsx` without crossing a `lazy()`
// boundary. Nothing in it may import a module that exists to be lazily loaded. Derived rather than
// listed, because a hand-maintained list of "the shell's modules" would go stale on the first new
// component and then pass forever.

import { describe, expect, it } from "vitest";

const sources: Record<string, string> = import.meta.glob("../src/**/*.{ts,tsx}", {
  query: "?raw",
  import: "default",
  eager: true,
});

/**
 * Modules that exist to be fetched on demand, so an eager module importing one defeats the split.
 *
 * The screens are reached only through `lazy(() => import(...))` in `App.tsx`, so they are covered
 * by the dynamic-import rule below rather than needing to be named. `kit.tsx` is named because it
 * is imported *statically* by thirty screens — that is what makes it a shared lazy chunk, and what
 * makes a single static import from the shell collapse it into the first paint.
 */
const LAZY_ONLY = ["components/kit"];

/**
 * The `from "…"` specifiers of a module's **value** imports.
 *
 * A dynamic `import()` is not one — that is the boundary this whole suite is about. Neither is
 * `import type`, which `verbatimModuleSyntax` erases entirely: `state/screens.ts` type-imports
 * `IconName` from a component, and counting that as a runtime edge would have this suite reporting
 * a bundle cost that does not exist.
 */
function staticImports(text: string): string[] {
  return [...text.matchAll(/^import\s(?!type\s)(?:[\s\S]*?)from\s+"([^"]+)";/gm)].map(
    (match) => match[1] as string,
  );
}

/** `../src/lib/theme.ts` → `lib/theme`, the form an import specifier resolves to. */
function moduleId(path: string): string {
  return path.replace(/^\.\.\/src\//, "").replace(/\.tsx?$/, "");
}

function resolve(fromId: string, specifier: string): string | undefined {
  if (!specifier.startsWith(".")) {
    return undefined; // a package, not one of ours
  }
  const parts = fromId.split("/").slice(0, -1);
  for (const segment of specifier.split("/")) {
    if (segment === "." || segment === "") {
      continue;
    }
    if (segment === "..") {
      parts.pop();
      continue;
    }
    parts.push(segment);
  }
  return parts.join("/");
}

const byId = new Map(Object.entries(sources).map(([path, text]) => [moduleId(path), text]));

/** Everything reachable from `main.tsx` through static imports only. */
function eagerGraph(): Set<string> {
  const seen = new Set<string>();
  const queue = ["main"];
  while (queue.length > 0) {
    const id = queue.pop() as string;
    if (seen.has(id)) {
      continue;
    }
    seen.add(id);
    const text = byId.get(id);
    if (text === undefined) {
      continue;
    }
    for (const specifier of staticImports(text)) {
      const target = resolve(id, specifier);
      if (target !== undefined && byId.has(target)) {
        queue.push(target);
      }
    }
  }
  return seen;
}

describe("the first-paint bundle", () => {
  it("is found, so this suite cannot pass by walking nothing", () => {
    const eager = eagerGraph();
    // Deliberately loose bounds: what matters is that the walk reached the shell and stopped short
    // of the forty screens, not the exact count on any given day.
    expect(eager.has("components/Shell")).toBe(true);
    expect(eager.has("components/AccountMenu")).toBe(true);
    expect(eager.size).toBeGreaterThan(8);
    expect(eager.size).toBeLessThan(40);
  });

  it("reaches only the screens an unauthenticated visitor can, not the forty behind the guard", () => {
    // `App.tsx` imports the three public screens eagerly on purpose: they are all a visitor who is
    // not signed in can reach, and lazily loading the login page would put a round trip in front of
    // typing a password. Named here rather than allowed by a `screens/` prefix rule, so a fourth
    // eager screen is a decision someone makes rather than one that slips in.
    const eager = [...eagerGraph()].filter((id) => id.startsWith("screens/")).sort();
    expect(eager).toEqual(["screens/AcceptInvite", "screens/Login", "screens/Setup"]);
  });

  it("does not statically import a module meant to load lazily", () => {
    const eager = eagerGraph();
    const offenders: string[] = [];
    for (const id of eager) {
      const text = byId.get(id);
      if (text === undefined) {
        continue;
      }
      for (const specifier of staticImports(text)) {
        const target = resolve(id, specifier);
        if (target !== undefined && LAZY_ONLY.includes(target)) {
          offenders.push(`${id} imports ${target}`);
        }
      }
    }
    expect(offenders).toEqual([]);
  });
});
