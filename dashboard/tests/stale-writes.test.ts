// A screen that sends an `etag` must handle the refusal it invites (Wave 3 · Stage 5).
//
// # The defect this was written from
//
// ADR-0094 makes an `/admin` edit conditional: the console sends the version it read, and the server
// answers `412` when somebody else saved first. Fourteen screens explained that and reloaded. Four —
// Activation, Campaigns, Inventory and ReasonCodes — sent the `etag` and never looked at the answer,
// so a concurrent-edit conflict reached the operator as the raw server message and the screen kept
// showing the stale record they had just failed to overwrite.
//
// Nobody chose that. `isStale` lived in `screens/catalog/shared.tsx`, so it was reachable from the
// five catalog screens and invisible to everyone else; whoever wrote the other screens wrote the
// generic ternary instead, forty-three times. Moving the two helpers to `lib/errors.ts` is what
// makes the fix available, and this is what makes it stay: the next screen to send an `etag` will
// forget too, and the failure it causes is invisible until two people edit at once in production.
//
// # Why it reads the source
//
// The property is "this screen's failure path consults `isStale`", which is a property of the code
// rather than of one rendered interaction. Proving it by rendering would mean driving a concurrent
// edit through fifteen screens with mocked `412`s to assert something the source states plainly.

import { describe, expect, it } from "vitest";

const sources: Record<string, string> = import.meta.glob("../src/screens/**/*.tsx", {
  query: "?raw",
  import: "default",
  eager: true,
});

/**
 * The file with its comments removed.
 *
 * Written after the first version of this suite passed a deliberately broken screen: the predicate
 * was `text.includes("isStale")`, and the explanatory comment *above* the broken failure path
 * contained the word. A check that prose can satisfy checks nothing. Trailing `//` is only stripped
 * when it is not part of a `://`, so a URL in a string survives.
 */
function code(text: string): string {
  return text
    .replace(/\/\*[\s\S]*?\*\//g, "")
    .split("\n")
    .map((line) => {
      const trimmed = line.trimStart();
      // An import is not a use. Dropped for the same reason comments are: reverting a screen's
      // stale branch while leaving its import in place kept this suite green, which is the second
      // way it has been caught claiming a protection it did not have.
      if (trimmed.startsWith("//") || trimmed.startsWith("import ")) {
        return "";
      }
      return line.replace(/(?<!:)\/\/.*$/, "");
    })
    .join("\n");
}

/** Every screen as written — imports included, which is what an adoption check must read. */
const raw = Object.entries(sources);

/** Every screen with its comments and imports removed — what a "does the code do this" check reads. */
const entries = raw.map(([path, text]) => [path, code(text)] as [string, string]);

/** A screen that passes a read version back on a write — ADR-0094's conditional write. */
const sendsEtag = ([, text]: [string, string]) => /\betag\b/i.test(text);

/**
 * A screen that tells the two failures apart.
 *
 * Two spellings, both through `lib/errors.ts`, and the second is the one to prefer:
 *
 *   * `isStale(caught)` in the screen's own `catch` — how the fourteen original screens do it;
 *   * `withStaleReload(write, load, prose)` wrapping the write — which does the whole pattern
 *     (reload, then re-throw the refusal in the screen's own words) in the one place that knows it.
 *
 * The second exists because the first is only half of the obligation. Moving a screen onto
 * `EntityCrud` (ADR-0121) moves the `catch` out of the screen and into `run`, and the first screen
 * migrated that way kept the reload and silently dropped the prose: `stores.stale` was still in the
 * catalogue and no longer reachable. `withStaleReload` is what makes both halves travel together, so
 * a screen that uses it is handling the refusal even though the word `isStale` never appears in it.
 *
 * What must not happen is neither. Matching bare words is only safe because `code()` has removed the
 * comments *and* the imports: two earlier versions of this suite passed a deliberately broken
 * screen, the first because the comment explaining the failure path contained the word, the second
 * because the now-unused import still did. A check that prose or a leftover import can satisfy
 * checks nothing.
 */
const handlesStale = ([, text]: [string, string]) =>
  text.includes("isStale") || text.includes("withStaleReload");

describe("conditional writes", () => {
  it("finds the screens, so this suite cannot pass by looking at nothing", () => {
    expect(raw.length).toBeGreaterThan(20);
    expect(entries.filter(sendsEtag).length).toBeGreaterThanOrEqual(10);
  });

  it("are handled wherever they are made", () => {
    const unhandled = entries
      .filter(sendsEtag)
      .filter((entry) => !handlesStale(entry))
      .map(([path]) => path);
    expect(unhandled).toEqual([]);
  });

  it("name their own subject in the message, rather than sharing a generic one", () => {
    // Eight screens each say what changed — "this store", "these tax rates", "this area or table".
    // A shared `common.stale` would read "this record" everywhere, which is worse writing for a
    // smaller catalogue, so the four screens fixed here got their own keys in that house style.
    // This pins the convention: a stale branch cites a key of its own screen's namespace.
    const generic = entries
      .filter(handlesStale)
      .filter(([, text]) => text.includes('t("common.stale")'))
      .map(([path]) => path);
    expect(generic).toEqual([]);
  });

  it("keep their own prose when the reload moves into the shared helper", () => {
    // `withStaleReload` takes the sentence as its third argument, which makes the omission the
    // Stores migration actually committed — reload kept, prose dropped — a thing a test can see.
    // Anything that delegates must name a `*.stale` key of its own namespace in the same call.
    const silent = entries
      .filter(([, text]) => text.includes("withStaleReload"))
      .filter(([, text]) => !/withStaleReload\([\s\S]{0,200}?t\("[A-Za-z]+\.stale"\)/.test(text))
      .map(([path]) => path);
    expect(silent).toEqual([]);
  });
});

describe("the shared error helpers", () => {
  it("own the ApiError ternary, and no screen writes its own copy", () => {
    // Forty-three copies when `lib/errors.ts` was introduced; four screens adopted it then, and this
    // check recorded the direction rather than failing on the rest. The sweep is done, so the
    // assertion is now absolute: zero copies, in any screen, under any variable name.
    //
    // Two `instanceof ApiError` uses survive and are not copies of this expression: `Login.tsx`
    // deliberately shows a *generic* message rather than the server's (naming what was wrong about a
    // sign-in tells a guesser which half to vary), and `Fleet.tsx` falls back to a screen-specific
    // sentence. The pattern below is the ternary itself, so neither matches.
    const copies = entries
      .filter(([, text]) => /\b(\w+) instanceof ApiError \? \1\.message : String\(\1\)/.test(text))
      .map(([path]) => path);
    expect(copies).toEqual([]);
  });

  it("are the only definition, so a second one cannot drift from the first", () => {
    // `catalog/shared.tsx` defined `isStale` before `lib/errors.ts` existed — which is *why* the
    // screens outside `catalog/` wrote the ternary by hand. It re-exports the one definition now.
    // A blind sweep is what makes this worth pinning: rewriting the ternary everywhere turned that
    // second definition into `isStale = (caught) => isStale(caught)` — infinite recursion, which
    // TypeScript compiles without complaint.
    const definitions = raw
      .filter(([, text]) => /const isStale\s*=/.test(text))
      .map(([path]) => path);
    expect(definitions).toEqual([]);
  });
});
