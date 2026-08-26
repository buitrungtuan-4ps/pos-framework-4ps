# WCAG-AA contrast audit — design tokens

**Status** Accepted · **Owner** @maintainers-domain · **Last reviewed** 2026-08-26

This is the numeric backing for the "WCAG AA contrast" exit criterion of P6 (`docs/roadmap.md`) and
the semantic-colour rule in `docs/ui-ux.md` §1. It records the measured contrast ratio of every
colour pair the interface actually renders, in both themes, so "AA contrast" is a checked number
rather than a claim. The palette lives in `ui/src/styles/tokens.css` and (identically, pending the
dedup in WS-E) `dashboard/src/styles/tokens.css`.

## How the numbers are produced

The tokens are authored in `oklch(L C H)`. The audit converts each to sRGB with the standard Oklab
matrix, then applies the WCAG 2.1 relative-luminance and contrast-ratio formulae. It is not a
hand-estimate: `ui/scripts/wcag-contrast.mjs` (and its copy in `dashboard/scripts/`) parses the
`:root` (light) and `:root[data-theme="dark"]` (explicit dark) blocks straight out of `tokens.css`,
so the check tracks the tokens and cannot drift. Run it with `pnpm contrast`; the `build` script and
the `ui`/`dashboard` CI jobs invoke it, and it **exits non-zero if any text pair falls below AA
4.5:1** — so a token edit that breaks text contrast fails the build.

WCAG AA thresholds used: **4.5:1** for normal-size text, **3.0:1** for large text (≥ 24 px, or
≥ 18.66 px bold) and for non-text UI components (WCAG 1.4.11). The audit gates on the strict 4.5:1
for every token used as text, regardless of the size it happens to render at, so the guarantee holds
even where a token is later reused at a smaller size.

## Light theme

| Foreground | Background | Kind | Ratio | AA target | Pass |
|---|---|---|---|---|---|
| `ink` | `canvas` | text | 16.35:1 | 4.5:1 | ✅ |
| `ink` | `surface` | text | 17.31:1 | 4.5:1 | ✅ |
| `ink` | `surface-raised` | text | 17.07:1 | 4.5:1 | ✅ |
| `ink-muted` | `canvas` | text | 6.17:1 | 4.5:1 | ✅ |
| `ink-muted` | `surface` | text | 6.54:1 | 4.5:1 | ✅ |
| `ink-muted` | `surface-raised` | text | 6.44:1 | 4.5:1 | ✅ |
| `accent-ink` | `accent` | text | 5.11:1 | 4.5:1 | ✅ |
| `danger-ink` | `danger` | text | 5.20:1 | 4.5:1 | ✅ |
| `accent` | `surface` | text | 5.26:1 | 4.5:1 | ✅ |
| `accent` | `canvas` | text | 4.97:1 | 4.5:1 | ✅ |
| `danger` | `surface` | text | 5.35:1 | 4.5:1 | ✅ |
| `ok` | `surface` | text | 5.16:1 | 4.5:1 | ✅ |
| `line` | `surface` | non-text | 1.35:1 | — (exempt) | see below |
| `free` | `canvas` | non-text | 2.52:1 | — (exempt) | see below |
| `occupied` | `canvas` | non-text | 3.66:1 | — (exempt) | see below |
| `awaiting` | `canvas` | non-text | 2.40:1 | — (exempt) | see below |
| `cleaning` | `canvas` | non-text | 2.85:1 | — (exempt) | see below |

## Dark theme

| Foreground | Background | Kind | Ratio | AA target | Pass |
|---|---|---|---|---|---|
| `ink` | `canvas` | text | 16.53:1 | 4.5:1 | ✅ |
| `ink` | `surface` | text | 15.31:1 | 4.5:1 | ✅ |
| `ink` | `surface-raised` | text | 13.83:1 | 4.5:1 | ✅ |
| `ink-muted` | `canvas` | text | 7.71:1 | 4.5:1 | ✅ |
| `ink-muted` | `surface` | text | 7.14:1 | 4.5:1 | ✅ |
| `ink-muted` | `surface-raised` | text | 6.45:1 | 4.5:1 | ✅ |
| `accent-ink` | `accent` | text | 5.03:1 | 4.5:1 | ✅ |
| `danger-ink` | `danger` | text | 5.37:1 | 4.5:1 | ✅ |
| `accent` | `surface` | text | 4.53:1 | 4.5:1 | ✅ |
| `accent` | `canvas` | text | 4.89:1 | 4.5:1 | ✅ |
| `danger` | `surface` | text | 4.84:1 | 4.5:1 | ✅ |
| `ok` | `surface` | text | 6.51:1 | 4.5:1 | ✅ |
| `line` | `surface` | non-text | 1.40:1 | — (exempt) | see below |
| `free` | `canvas` | non-text | 3.94:1 | 3.0:1 | ✅ |
| `occupied` | `canvas` | non-text | 5.34:1 | 3.0:1 | ✅ |
| `awaiting` | `canvas` | non-text | 7.53:1 | 3.0:1 | ✅ |
| `cleaning` | `canvas` | non-text | 5.86:1 | 3.0:1 | ✅ |

## Findings and resolutions

**Every text pair clears AA 4.5:1 in both themes.** One finding was fixed as part of this audit:

- **`ok` in the light theme.** At its original `oklch(0.6 0.13 155)` the "success" green measured
  **3.72:1** on `surface` and 3.51:1 on `canvas` — below 4.5:1, and `ok` is used as small/normal text
  (`text-sm` on the fired-line badge, the shift-variance line, the paired/settled confirmations). It
  was darkened to **`oklch(0.52 0.13 155)`**, giving 5.16:1 / 4.88:1 — comfortably AA. The dark-theme
  `ok` already passed (6.51:1) and is unchanged.

## Why the non-text tokens are exempt, not failures

Three tokens measure below 3:1 in the light theme. Neither is a text colour, and both are exempt from
WCAG 1.4.11 (Non-text Contrast) because the information they carry never depends on the colour:

- **`line` (1.35–1.40:1)** is the 1 px separator/border. A border is not the *sole* means of
  identifying any control here — cards and inputs are also set apart by a surface-fill step
  (`surface` / `surface-raised` / `canvas`) and by their text — so the border is decorative. WCAG
  1.4.11 exempts purely decorative boundaries. Raising it to 3:1 would draw a heavy grid the
  "minimal frame" direction (`docs/ui-ux.md`) explicitly rejects.

- **Table-state fills `free` / `awaiting` / `cleaning` (2.40–2.85:1 on the light canvas)** render only
  as a 10 px `rounded-full` status **dot** that is marked `aria-hidden="true"` and always sits beside
  a visible text label (`Floor.tsx`, `StatusBar.tsx`). The colour is redundant with the label, which
  satisfies WCAG 1.4.1 (Use of Colour — never meaning by hue alone, `docs/ui-ux.md` §1), and a
  redundant graphic is not "required to understand the content", which is the 1.4.11 exemption. The
  same dots all clear 3:1 in the dark theme anyway. `occupied` clears 3:1 in both themes.

If any of these tokens is ever promoted to a text colour or made the sole indicator of a control,
add it to the gated `text` set in `wcag-contrast.mjs` and re-tune its lightness to 4.5:1.
