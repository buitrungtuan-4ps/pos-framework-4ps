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
 * Two spellings count, because the console has both: `isStale(caught)` through `lib/errors.ts`, and
 * the inline `caught instanceof ApiError && caught.isStale` that six screens wrote before the helper
 * had a shared home. Both are the code telling the two failures apart, which is the property; which
 * spelling is the mechanical follow-up. What must not happen is neither.
 *
 * Matching the bare word is only safe because `code()` has removed the comments *and* the imports.
 * Two earlier versions of this suite passed a deliberately broken screen: the first because the
 * comment explaining the failure path contained the word, the second because the now-unused import
 * still did. A check that prose or a leftover import can satisfy checks nothing.
 */
const handlesStale = ([, text]: [string, string]) => text.includes("isStale");

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
      .filter(([, text]) => text.includes("isStale"))
      .filter(([, text]) => text.includes('t("common.stale")'))
      .map(([path]) => path);
    expect(generic).toEqual([]);
  });
});

describe("the shared error helpers", () => {
  it("own the ApiError ternary, so screens stop rewriting it", () => {
    // Forty-three copies at the time of writing. The four screens this slice touched are the
    // beachhead; the rest are a mechanical follow-up, and this records the direction rather than
    // failing on the copies that remain.
    const copies = entries.filter(([, text]) =>
      text.includes("caught instanceof ApiError ? caught.message : String(caught)"),
    );
    // `raw`, not `entries`: importing the helper is exactly the line `code()` strips.
    const adopters = raw.filter(([, text]) => text.includes('from "../lib/errors"'));
    expect(adopters.length).toBeGreaterThanOrEqual(4);
    // None of the adopters kept a hand-written copy alongside the helper they now import.
    const both = adopters.filter(([path]) => copies.some(([other]) => other === path));
    expect(both.map(([path]) => path)).toEqual([]);
  });
});
