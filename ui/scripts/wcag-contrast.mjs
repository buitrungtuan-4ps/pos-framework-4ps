// The WCAG-AA contrast gate for the design tokens (P6 exit criterion, docs/wcag-contrast-audit.md).
//
// Parses `src/styles/tokens.css` for the oklch colour tokens in the light (`:root`) and explicit-dark
// (`:root[data-theme="dark"]`) palettes, converts each to sRGB (the standard Oklab matrix), and
// computes the WCAG 2.1 contrast ratio for every meaningful foreground/background pair. Text pairs
// must clear AA 4.5:1; a failure exits non-zero so the build fails. Non-text pairs (a 1px separator,
// the redundant aria-hidden state dots that always ride with a text label) are reported but never
// gate — they are exempt under WCAG 1.4.11, as the audit doc records. Run by `pnpm contrast`, which
// the `build` script and the `ui` CI job invoke.

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
const themes = {
  Light: palette(ruleBody(css, ":root {")),
  Dark: palette(ruleBody(css, ':root[data-theme="dark"] {')),
};

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
  ["accent", "surface", "text"],
  ["accent", "canvas", "text"],
  ["danger", "surface", "text"],
  ["ok", "surface", "text"],
  // Non-text, exempt under WCAG 1.4.11 — reported for the record, never gated.
  ["line", "surface", "ui"],
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
console.log("\nwcag-contrast: ok — every text pair clears AA 4.5:1 in both themes.");
