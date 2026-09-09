// The WCAG-AA contrast gate for the design tokens (P6 exit criterion, docs/wcag-contrast-audit.md).
//
// Parses `src/styles/tokens.css` for the oklch colour tokens in all three palettes — light
// (`:root`), system-dark (the `prefers-color-scheme` block) and explicit-dark
// (`:root[data-theme="dark"]`) — converts each to sRGB (the standard Oklab matrix), and
// computes the WCAG 2.1 contrast ratio for every meaningful foreground/background pair. Text pairs
// must clear AA 4.5:1; a failure exits non-zero so the build fails. Non-text pairs (a 1px separator,
// the redundant aria-hidden state dots that always ride with a text label) are reported but never
// gate — they are exempt under WCAG 1.4.11, as the audit doc records. Run by `pnpm contrast`, which
// the `build` script and the `ui` CI job invoke.
//
// # Why all three blocks, and why they are checked against each other
//
// This gate used to read two of the three. The one it skipped is the `prefers-color-scheme: dark`
// block — which is the palette a viewer who has *not* chosen a theme actually gets, so the default
// dark experience was the one palette never audited. It passed only because it duplicates the
// explicit-dark block verbatim, and nothing checked that it still did. Editing one of the two and
// not the other would have shipped an unaudited palette with no failure anywhere.
//
// So: every block is audited, and before that the three are checked against each other. The names
// must match in all three — `tokens.css` states that invariant in prose ("Every colour token is
// defined here so none has its only value in a media query") and nothing enforced it — and the two
// dark blocks must agree on values, which is what makes duplicating them safe.

import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";

const TOKENS = fileURLToPath(new URL("../src/styles/tokens.css", import.meta.url));

// --- oklch → sRGB → WCAG relative luminance → contrast ratio -------------------------------------

function oklchToSrgb(L, C, H) {
  const h = (H * Math.PI) / 180;
  const a = C * Math.cos(h);
  const b = C * Math.sin(h);
  const l_ = L + 0.3963377774 * a + 0.2158037573 * b;
  const m_ = L - 0.1055613458 * a - 0.0638541728 * b;
  const s_ = L - 0.0894841775 * a - 1.291485548 * b;
  const l = l_ ** 3;
  const m = m_ ** 3;
  const s = s_ ** 3;
  const enc = (c) => {
    const x = Math.min(1, Math.max(0, c));
    return x <= 0.0031308 ? 12.92 * x : 1.055 * x ** (1 / 2.4) - 0.055;
  };
  return [
    enc(4.0767416621 * l - 3.3077115913 * m + 0.2309699292 * s),
    enc(-1.2684380046 * l + 2.6097574011 * m - 0.3413193965 * s),
    enc(-0.0041960863 * l - 0.7034186147 * m + 1.707614701 * s),
  ];
}

function luminance([r, g, b]) {
  const lin = (c) => (c <= 0.04045 ? c / 12.92 : ((c + 0.055) / 1.055) ** 2.4);
  return 0.2126 * lin(r) + 0.7152 * lin(g) + 0.0722 * lin(b);
}

function ratio(fg, bg) {
  const a = luminance(fg) + 0.05;
  const b = luminance(bg) + 0.05;
  return Math.max(a, b) / Math.min(a, b);
}

// --- parse the token palettes out of tokens.css --------------------------------------------------

// Returns the `{ ... }` body of the first rule whose selector matches `selector` exactly.
function ruleBody(css, selector) {
  const start = css.indexOf(selector);
  if (start < 0) {
    throw new Error(`tokens.css has no \`${selector}\` rule`);
  }
  const open = css.indexOf("{", start);
  const close = css.indexOf("}", open);
  return css.slice(open + 1, close);
}

// Every custom property a rule body declares, in name order. Unlike `palette` below this does not
// care what the value is, so it sees any non-colour runtime token too — a shadow, say, which is
// re-pointed per theme for exactly the same reason a colour is.
function declaredNames(body) {
  return [...body.matchAll(/(--[\w-]+)\s*:/g)].map((match) => match[1]).sort();
}

// A rule body with its comments stripped and its whitespace flattened — the form in which two blocks
// that say the same thing compare equal regardless of indentation or the prose around them.
function normalised(body) {
  return body.replace(/\/\*[\s\S]*?\*\//g, " ").replace(/\s+/g, " ").trim();
}

// Extracts `--name: oklch(L C H)` (and `oklch(1 0 0)` shorthands) from a rule body.
function palette(body) {
  const tokens = {};
  const re = /--([\w-]+):\s*oklch\(\s*([\d.]+)\s+([\d.]+)\s+([\d.]+)/g;
  let match;
  while ((match = re.exec(body)) !== null) {
    tokens[match[1]] = [Number(match[2]), Number(match[3]), Number(match[4])];
  }
  return tokens;
}

const css = readFileSync(TOKENS, "utf8");

// Named rather than discovered, so deleting or renaming a block fails here instead of quietly
// shrinking what this gate audits.
const BLOCKS = {
  Light: ":root {",
  "Dark (system)": ':root:not([data-theme="light"]) {',
  "Dark (chosen)": ':root[data-theme="dark"] {',
};
const bodies = Object.fromEntries(
  Object.entries(BLOCKS).map(([name, selector]) => [name, ruleBody(css, selector)]),
);

// --- the three blocks agree with each other ------------------------------------------------------

const [reference, ...others] = Object.keys(bodies);
for (const name of others) {
  const expected = declaredNames(bodies[reference]);
  const found = declaredNames(bodies[name]);
  const missing = expected.filter((token) => !found.includes(token));
  const extra = found.filter((token) => !expected.includes(token));
  if (missing.length > 0 || extra.length > 0) {
    console.error(`\nwcag-contrast: \`${name}\` does not declare the same tokens as \`${reference}\``);
    if (missing.length > 0) {
      console.error(`  missing: ${missing.join(", ")}`);
    }
    if (extra.length > 0) {
      console.error(`  only here: ${extra.join(", ")}`);
    }
    process.exit(1);
  }
}

// The two dark blocks are a deliberate duplication — CSS gives no way to point one at the other
// without a third indirection per token — so what keeps it honest is checking it.
if (normalised(bodies["Dark (system)"]) !== normalised(bodies["Dark (chosen)"])) {
  console.error(
    "\nwcag-contrast: the system-dark and chosen-dark blocks have drifted apart.\n" +
      "  They must hold identical values: a viewer who has chosen dark and one whose system is dark\n" +
      "  see the same product, and this gate audits the pair as one palette.",
  );
  process.exit(1);
}

const themes = Object.fromEntries(
  Object.entries(bodies).map(([name, body]) => [name, palette(body)]),
);

// --- the pairs the interface actually renders ----------------------------------------------------
// kind "text": normal-size text, AA 4.5:1 (gates). "ui": non-text, exempt here (reported only).

const PAIRS = [
  ["ink", "canvas", "text"],
  ["ink", "surface", "text"],
  ["ink", "surface-raised", "text"],
  ["ink-muted", "canvas", "text"],
  ["ink-muted", "surface", "text"],
  ["ink-muted", "surface-raised", "text"],
  ["accent-ink", "accent", "text"],
  ["danger-ink", "danger", "text"],
  // The open nav row: `selected-ink` on `selected`. Gated as text because that is what it is — the
  // row's label — and it is the pair most easily got wrong, since a tint pale enough to sit under a
  // whole row is also pale enough to lose its foreground.
  ["selected-ink", "selected", "text"],
  ["accent", "surface", "text"],
  ["accent", "canvas", "text"],
  ["danger", "surface", "text"],
  ["ok", "surface", "text"],
  // Non-text, exempt under WCAG 1.4.11 — reported for the record, never gated.
  ["line", "surface", "ui"],
  // The dot a closed group wears when it holds the open screen. Non-text, and it rides with a
  // label, so it is reported rather than gated — same standing as the readiness marker.
  ["selected-ink", "surface", "ui"],
  ["free", "canvas", "ui"],
  ["occupied", "canvas", "ui"],
  ["awaiting", "canvas", "ui"],
  ["cleaning", "canvas", "ui"],
];

const AA_TEXT = 4.5;
let failures = 0;

for (const [theme, tokens] of Object.entries(themes)) {
  console.log(`\n${theme}`);
  for (const [fg, bg, kind] of PAIRS) {
    if (tokens[fg] === undefined || tokens[bg] === undefined) {
      throw new Error(`${theme}: token \`${fg}\` or \`${bg}\` missing from tokens.css`);
    }
    const r = ratio(oklchToSrgb(...tokens[fg]), oklchToSrgb(...tokens[bg]));
    const gated = kind === "text";
    const ok = !gated || r >= AA_TEXT;
    if (!ok) {
      failures += 1;
    }
    const flag = gated ? (ok ? "ok " : "FAIL") : "n/t";
    console.log(`  ${flag}  ${fg} on ${bg}: ${r.toFixed(2)}:1${gated ? ` (need ${AA_TEXT})` : ""}`);
  }
}

if (failures > 0) {
  console.error(`\nwcag-contrast: ${failures} text pair(s) below AA 4.5:1`);
  process.exit(1);
}
console.log("\nwcag-contrast: ok — every text pair clears AA 4.5:1 in all three palettes.");
