// Elevation and motion are tokens, and a token nothing consumes is a comment (Wave 3 · Stage 6).
//
// # The two defects this was written from
//
// `--ease-token` had been declared in `@theme` since P6 and had **zero** consumers. `Button` — the
// one component in the console that animated anything — wrote `ease-[cubic-bezier(0.2,0,0,1)]`, the
// token's exact value, longhand as an arbitrary Tailwind class, beside a bare `duration-150`. The
// token existed, its value was duplicated, and `tokens.css`'s own promise that a utility "resolves
// to a token, never a magic number" was not true of the only motion in the product.
//
// Elevation had no token at all. Six floating surfaces — both modal shapes, the command palette,
// the org-switcher dropdown, the notification dropdown, the toast — reached for Tailwind's default
// `shadow-lg`, which is `rgb(0 0 0 / 0.1)`. On the light palette that is a shadow. On the dark one,
// where `--canvas` is `oklch(0.17 …)`, a ten-percent black shadow over a near-black ground is
// nothing at all, so in dark mode every overlay in the console lost its depth and read as a flat
// patch of slightly different grey. That is a rendering defect, not a preference: one class name
// was correct in one theme and inert in the other, which is exactly the failure the colour tokens
// exist to prevent — and elevation was the last visual property still outside them.
//
// # What this checks, and what checks the rest
//
// The half that can be read here is the components: no `.tsx` reaches past the tokens, and the two
// elevation steps have consumers, so this cannot pass by there being no shadows left to draw.
//
// The other half — that every runtime variable is defined in all three theme blocks, and that the
// two dark blocks still agree — lives in `scripts/wcag-contrast.mjs`, which already parses
// `tokens.css` and gates on it. Not by preference: Vitest stubs CSS imports (`css: false`), so a
// `?raw` import of a stylesheet resolves to an empty string here and any assertion about its
// contents would pass vacuously. A check that cannot see its subject is worse than no check.

import { describe, expect, it } from "vitest";

const components: Record<string, string> = import.meta.glob("../src/**/*.tsx", {
  query: "?raw",
  import: "default",
  eager: true,
});

/** Every `.tsx` under `src/`, as written. A class name is a string literal; no parsing is needed. */
const files = Object.entries(components);

/**
 * Every `class` value in a file, one string per attribute.
 *
 * Both spellings the console uses: a plain `class="…"` and a `class={`…`}` template, whose `${…}`
 * interpolations hold no backtick and so do not end the match. A value assembled in a helper
 * function is not seen — none of them carries a `hover:`, and a check that pretended otherwise
 * would be claiming a reach it does not have.
 */
function classValues(text: string): string[] {
  const plain = [...text.matchAll(/class="([^"]*)"/g)].map((match) => match[1] as string);
  const template = [...text.matchAll(/class=\{`([^`]*)`/g)].map((match) => match[1] as string);
  return [...plain, ...template];
}

/** Tailwind's own shadow scale — the untokenised one, which does not flip with the theme. */
const DEFAULT_SHADOWS = /\b(?:drop-)?shadow-(?:2xs|xs|sm|md|lg|xl|2xl|inner|none)\b/;

/** An easing or a duration written at the call site rather than taken from the tokens. */
const OWN_MOTION = /\bease-\[|\bduration-\d/;

describe("no component", () => {
  it("finds the components, so this suite cannot pass by looking at nothing", () => {
    expect(files.length).toBeGreaterThan(40);
  });

  it("reaches past the tokens to Tailwind's default shadow scale", () => {
    const offenders = files.filter(([, text]) => DEFAULT_SHADOWS.test(text)).map(([path]) => path);
    expect(offenders).toEqual([]);
  });

  it("writes its own easing or duration", () => {
    // Both come from `--ease-token` and `--default-transition-duration` now, so a site that wants
    // the house motion writes a bare `transition-colors` and nothing else. A site that writes its
    // own number is a site the token cannot reach.
    const offenders = files.filter(([, text]) => OWN_MOTION.test(text)).map(([path]) => path);
    expect(offenders).toEqual([]);
  });
});

describe("the elevation tokens", () => {
  it("have consumers, so the theme blocks are not defining a shadow nothing draws", () => {
    const overlays = files.filter(([, text]) => text.includes("shadow-overlay"));
    const raised = files.filter(([, text]) => text.includes("shadow-raised"));
    // Six floating surfaces across four modules: the two modal shapes, the palette, the two
    // dropdowns, and the toast.
    expect(overlays.length).toBeGreaterThanOrEqual(4);
    // `Card`, which every screen renders.
    expect(raised.length).toBeGreaterThanOrEqual(1);
  });
});

describe("every hover state that changes a colour", () => {
  it("transitions, so the token's easing has something to ease", () => {
    // A `hover:` that changes a colour and does not transition snaps. Sixteen of them did, which is
    // part of why the motion token went unnoticed as inert: there was almost nothing for it to ease.
    //
    // Checked per class value rather than per file. A file-level check ("this file mentions
    // `transition` somewhere") passes a file whose *second* hover site was missed, which is the
    // shape this defect actually had — one component with the class and eleven without.
    //
    // Only colour changes. `Layout.tsx` has the one hover that is not a colour — it adds an
    // underline — and is deliberately outside this rule rather than exempted from it: a
    // text-decoration appearing has nothing to interpolate, so a transition there would be a class
    // that reads as motion and produces none.
    const CHANGES_COLOUR = /hover:(?:text|bg|border)-/;
    const offenders = files.flatMap(([path, text]) =>
      classValues(text)
        .filter((value) => CHANGES_COLOUR.test(value))
        .filter((value) => !value.includes("transition"))
        .map((value) => `${path}: ${value.slice(0, 60)}`),
    );
    expect(offenders).toEqual([]);
  });
});
