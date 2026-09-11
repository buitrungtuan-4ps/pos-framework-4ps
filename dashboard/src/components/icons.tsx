// The console's icon set (Wave 3 · Stage 6).
//
// # Why the geometry is vendored rather than drawn or imported
//
// The nav carries thirty entries. Icons are what let an operator stop reading the words for the two
// screens they use every day — but only if they are consistent: thirty glyphs at thirty different
// stroke weights and optical sizes are noise that makes the recognisable ones harder to find, not
// easier. Hand-drawing a coherent set of thirty is a designer's job, and hand-drawing an incoherent
// one would be worse than the plain text it replaced. That is why
// [#261](https://github.com/buitrungtuan-4ps/pos-framework-4ps/pull/261) collapsed the nav and
// deliberately left the icons out.
//
// Importing an icon package would be the ordinary answer, and `AGENTS.md` §2 forbids it without an
// ADR merged first — correctly, because a runtime dependency is a supply chain. So the path data
// below is **transcribed** from Lucide's published SVGs by a generator, checked in as source, and
// carries the notice its licence requires. No entry in `package.json`, nothing in the lockfile,
// nothing in `node_modules`, and only the glyphs actually used ship in the bundle — one per screen,
// and no more. The nav group headings used to carry one each too; they no longer do (see
// `state/screens.ts` for why), and the five glyphs that only they drew went with them, because a
// glyph nobody draws is bundle weight with nothing to show for it.
//
// Regenerating: `npm pack lucide-static@<version>` outside the repository, then re-run the
// generator recorded in `docs/cloud-admin-ux-plan.md`. Editing a `d` attribute by hand is the one
// thing not to do — the value is a transcription, and a transcription that has been touched is just
// a drawing with a false provenance.
//
// ---
//
// Glyph geometry from Lucide (https://lucide.dev), used under the ISC License:
//
//   ISC License
//
//   Copyright (c) 2026 Lucide Icons and Contributors
//
//   Permission to use, copy, modify, and/or distribute this software for any purpose with or
//   without fee is hereby granted, provided that the above copyright notice and this permission
//   notice appear in all copies.
//
//   THE SOFTWARE IS PROVIDED "AS IS" AND THE AUTHOR DISCLAIMS ALL WARRANTIES WITH REGARD TO THIS
//   SOFTWARE INCLUDING ALL IMPLIED WARRANTIES OF MERCHANTABILITY AND FITNESS. IN NO EVENT SHALL THE
//   AUTHOR BE LIABLE FOR ANY SPECIAL, DIRECT, INDIRECT, OR CONSEQUENTIAL DAMAGES OR ANY DAMAGES
//   WHATSOEVER RESULTING FROM LOSS OF USE, DATA OR PROFITS, WHETHER IN AN ACTION OF CONTRACT,
//   NEGLIGENCE OR OTHER TORTIOUS ACTION, ARISING OUT OF OR IN CONNECTION WITH THE USE OR
//   PERFORMANCE OF THIS SOFTWARE.

import type { JSX } from "solid-js";

/** Every glyph the console has. A name outside this union does not compile. */
export const ICON_NAMES = [
  "armchair",
  "arrow-down-to-line",
  "book-open",
  "chart-column",
  "chef-hat",
  "clipboard-list",
  "credit-card",
  "file-cog",
  "gauge",
  "history",
  "image",
  "key",
  "languages",
  "layers",
  "layout-grid",
  "megaphone",
  "monitor-check",
  "monitor-smartphone",
  "package",
  "percent",
  "plug-zap",
  "plus-circle",
  "refresh-cw",
  "server",
  "settings",
  "shield-check",
  "shield-user",
  "store",
  "triangle-alert",
  "user-cog",
  "users",
  "webhook",
] as const;

export type IconName = (typeof ICON_NAMES)[number];

// The children of each 24x24 glyph. The wrapper `<svg>` in `Icon` carries every shared attribute —
// the viewBox, `fill: none`, `stroke: currentColor`, the 2px weight and the round joins — so a glyph
// here is only its geometry and inherits the colour of the text it sits beside.
const GLYPHS: Record<IconName, () => JSX.Element> = {
  "armchair": () => (
    <>
      <path d="M19 9V6a2 2 0 0 0-2-2H7a2 2 0 0 0-2 2v3" />
      <path d="M3 16a2 2 0 0 0 2 2h14a2 2 0 0 0 2-2v-5a2 2 0 0 0-4 0v1.5a.5.5 0 0 1-.5.5h-9a.5.5 0 0 1-.5-.5V11a2 2 0 0 0-4 0z" />
      <path d="M5 18v2" />
      <path d="M19 18v2" />
    </>
  ),
  "arrow-down-to-line": () => (
    <>
      <path d="M12 17V3" />
      <path d="m6 11 6 6 6-6" />
      <path d="M19 21H5" />
    </>
  ),
  "book-open": () => (
    <>
      <path d="M12 5v16" />
      <path d="M20.001 19A2 2 0 0022 17V5a2 2 0 00-1.999-2L16 3.002A5 5 0 0012 5a5 5 0 00-4-2H4a2 2 0 00-2 2v12a2 2 0 001.999 2H8a5 5 0 014 2 5 5 0 014-2z" />
    </>
  ),
  "chart-column": () => (
    <>
      <path d="M3 3v16a2 2 0 0 0 2 2h16" />
      <path d="M18 17V9" />
      <path d="M13 17V5" />
      <path d="M8 17v-3" />
    </>
  ),
  "chef-hat": () => (
    <>
      <path d="M17 21a1 1 0 0 0 1-1v-5.35c0-.457.316-.844.727-1.041a4 4 0 0 0-2.134-7.589 5 5 0 0 0-9.186 0 4 4 0 0 0-2.134 7.588c.411.198.727.585.727 1.041V20a1 1 0 0 0 1 1Z" />
      <path d="M6 17h12" />
    </>
  ),
  "clipboard-list": () => (
    <>
      <rect width="8" height="4" x="8" y="2" rx="1" ry="1" />
      <path d="M16 4h2a2 2 0 0 1 2 2v14a2 2 0 0 1-2 2H6a2 2 0 0 1-2-2V6a2 2 0 0 1 2-2h2" />
      <path d="M12 11h4" />
      <path d="M12 16h4" />
      <path d="M8 11h.01" />
      <path d="M8 16h.01" />
    </>
  ),
  "credit-card": () => (
    <>
      <rect width="20" height="14" x="2" y="5" rx="2" />
      <line x1="2" x2="22" y1="10" y2="10" />
    </>
  ),
  "file-cog": () => (
    <>
      <path d="M15 8a1 1 0 0 1-1-1V2a2.4 2.4 0 0 1 1.704.706l3.588 3.588A2.4 2.4 0 0 1 20 8z" />
      <path d="M20 8v12a2 2 0 0 1-2 2h-4.182" />
      <path d="m3.305 19.53.923-.382" />
      <path d="M4 10.592V4a2 2 0 0 1 2-2h8" />
      <path d="m4.228 16.852-.924-.383" />
      <path d="m5.852 15.228-.383-.923" />
      <path d="m5.852 20.772-.383.924" />
      <path d="m8.148 15.228.383-.923" />
      <path d="m8.53 21.696-.382-.924" />
      <path d="m9.773 16.852.922-.383" />
      <path d="m9.773 19.148.922.383" />
      <circle cx="7" cy="18" r="3" />
    </>
  ),
  "gauge": () => (
    <>
      <path d="m12 14 4-4" />
      <path d="M3.34 19a10 10 0 1 1 17.32 0" />
    </>
  ),
  "history": () => (
    <>
      <path d="M3 12a9 9 0 1 0 9-9 9.75 9.75 0 0 0-6.74 2.74L3 8" />
      <path d="M3 3v5h5" />
      <path d="M12 7v5l4 2" />
    </>
  ),
  "image": () => (
    <>
      <rect width="18" height="18" x="3" y="3" rx="2" ry="2" />
      <circle cx="9" cy="9" r="2" />
      <path d="m21 15-3.086-3.086a2 2 0 0 0-2.828 0L6 21" />
    </>
  ),
  "key": () => (
    <>
      <path d="m2 21 9.6-9.6" />
      <path d="m7.5 15.5 2.3 2.3a1 1 0 0 1 0 1.4l-2.1 2.1a1 1 0 0 1-1.4 0L4 19" />
      <circle cx="15.5" cy="7.5" r="5.5" />
    </>
  ),
  "languages": () => (
    <>
      <path d="m5 8 6 6" />
      <path d="m4 14 6-6 2-3" />
      <path d="M2 5h12" />
      <path d="M7 2h1" />
      <path d="m22 22-5-10-5 10" />
      <path d="M14 18h6" />
    </>
  ),
  "layout-grid": () => (
    <>
      <rect width="7" height="7" x="3" y="3" rx="1" />
      <rect width="7" height="7" x="14" y="3" rx="1" />
      <rect width="7" height="7" x="14" y="14" rx="1" />
      <rect width="7" height="7" x="3" y="14" rx="1" />
    </>
  ),
  "layers": () => (
    <>
      <path d="M12.83 2.18a2 2 0 0 0-1.66 0L2.6 6.08a1 1 0 0 0 0 1.83l8.58 3.91a2 2 0 0 0 1.66 0l8.58-3.9a1 1 0 0 0 0-1.83z" />
      <path d="M2 12.13a1 1 0 0 0 .58.91l8.6 3.91a2 2 0 0 0 1.65 0l8.58-3.9a1 1 0 0 0 .59-.92" />
      <path d="M2 16.87a1 1 0 0 0 .58.91l8.6 3.91a2 2 0 0 0 1.65 0l8.58-3.9a1 1 0 0 0 .59-.92" />
    </>
  ),
  "megaphone": () => (
    <>
      <path d="M11 6a13 13 0 0 0 8.4-2.8A1 1 0 0 1 21 4v12a1 1 0 0 1-1.6.8A13 13 0 0 0 11 14H5a2 2 0 0 1-2-2V8a2 2 0 0 1 2-2z" />
      <path d="M6 14a12 12 0 0 0 2.4 7.2 2 2 0 0 0 3.2-2.4A8 8 0 0 1 10 14" />
      <path d="M8 6v8" />
    </>
  ),
  "monitor-check": () => (
    <>
      <path d="m9 10 2 2 4-4" />
      <rect width="20" height="14" x="2" y="3" rx="2" />
      <path d="M12 17v4" />
      <path d="M8 21h8" />
    </>
  ),
  "monitor-smartphone": () => (
    <>
      <path d="M18 8V6a2 2 0 0 0-2-2H4a2 2 0 0 0-2 2v7a2 2 0 0 0 2 2h8" />
      <path d="M10 19v-3.96 3.15" />
      <path d="M7 19h5" />
      <rect width="6" height="10" x="16" y="12" rx="2" />
    </>
  ),
  "package": () => (
    <>
      <path d="M11 21.73a2 2 0 0 0 2 0l7-4A2 2 0 0 0 21 16V8a2 2 0 0 0-1-1.73l-7-4a2 2 0 0 0-2 0l-7 4A2 2 0 0 0 3 8v8a2 2 0 0 0 1 1.73z" />
      <path d="M12 22V12" />
      <polyline points="3.29 7 12 12 20.71 7" />
      <path d="m7.5 4.27 9 5.15" />
    </>
  ),
  "percent": () => (
    <>
      <line x1="19" x2="5" y1="5" y2="19" />
      <circle cx="6.5" cy="6.5" r="2.5" />
      <circle cx="17.5" cy="17.5" r="2.5" />
    </>
  ),
  "plug-zap": () => (
    <>
      <path d="M6.3 20.3a2.4 2.4 0 0 0 3.4 0L12 18l-6-6-2.3 2.3a2.4 2.4 0 0 0 0 3.4Z" />
      <path d="m2 22 3-3" />
      <path d="M7.5 13.5 10 11" />
      <path d="M10.5 16.5 13 14" />
      <path d="m18 3-4 4h6l-4 4" />
    </>
  ),
  "plus-circle": () => (
    <>
      <circle cx="12" cy="12" r="10" />
      <path d="M8 12h8" />
      <path d="M12 8v8" />
    </>
  ),
  "refresh-cw": () => (
    <>
      <path d="M3 12a9 9 0 0 1 9-9 9.75 9.75 0 0 1 6.74 2.74L21 8" />
      <path d="M21 3v5h-5" />
      <path d="M21 12a9 9 0 0 1-9 9 9.75 9.75 0 0 1-6.74-2.74L3 16" />
      <path d="M8 16H3v5" />
    </>
  ),
  "server": () => (
    <>
      <rect width="20" height="8" x="2" y="2" rx="2" ry="2" />
      <rect width="20" height="8" x="2" y="14" rx="2" ry="2" />
      <line x1="6" x2="6.01" y1="6" y2="6" />
      <line x1="6" x2="6.01" y1="18" y2="18" />
    </>
  ),
  "settings": () => (
    <>
      <path d="M9.671 4.136a2.34 2.34 0 0 1 4.659 0 2.34 2.34 0 0 0 3.319 1.915 2.34 2.34 0 0 1 2.33 4.033 2.34 2.34 0 0 0 0 3.831 2.34 2.34 0 0 1-2.33 4.033 2.34 2.34 0 0 0-3.319 1.915 2.34 2.34 0 0 1-4.659 0 2.34 2.34 0 0 0-3.32-1.915 2.34 2.34 0 0 1-2.33-4.033 2.34 2.34 0 0 0 0-3.831A2.34 2.34 0 0 1 6.35 6.051a2.34 2.34 0 0 0 3.319-1.915" />
      <circle cx="12" cy="12" r="3" />
    </>
  ),
  "shield-check": () => (
    <>
      <path d="M20 13c0 5-3.5 7.5-7.66 8.95a1 1 0 0 1-.67-.01C7.5 20.5 4 18 4 13V6a1 1 0 0 1 1-1c2 0 4.5-1.2 6.24-2.72a1.17 1.17 0 0 1 1.52 0C14.51 3.81 17 5 19 5a1 1 0 0 1 1 1z" />
      <path d="m9 12 2 2 4-4" />
    </>
  ),
  "shield-user": () => (
    <>
      <path d="M20 13c0 5-3.5 7.5-7.66 8.95a1 1 0 0 1-.67-.01C7.5 20.5 4 18 4 13V6a1 1 0 0 1 1-1c2 0 4.5-1.2 6.24-2.72a1.17 1.17 0 0 1 1.52 0C14.51 3.81 17 5 19 5a1 1 0 0 1 1 1z" />
      <path d="M6.376 18.91a6 6 0 0 1 11.249.003" />
      <circle cx="12" cy="11" r="4" />
    </>
  ),
  "store": () => (
    <>
      <path d="M15 21v-5a1 1 0 0 0-1-1h-4a1 1 0 0 0-1 1v5" />
      <path d="M17.774 10.31a1.12 1.12 0 0 0-1.549 0 2.5 2.5 0 0 1-3.451 0 1.12 1.12 0 0 0-1.548 0 2.5 2.5 0 0 1-3.452 0 1.12 1.12 0 0 0-1.549 0 2.5 2.5 0 0 1-3.77-3.248l2.889-4.184A2 2 0 0 1 7 2h10a2 2 0 0 1 1.653.873l2.895 4.192a2.5 2.5 0 0 1-3.774 3.244" />
      <path d="M4 10.95V19a2 2 0 0 0 2 2h12a2 2 0 0 0 2-2v-8.05" />
    </>
  ),
  "triangle-alert": () => (
    <>
      <path d="m21.73 18-8-14a2 2 0 0 0-3.48 0l-8 14A2 2 0 0 0 4 21h16a2 2 0 0 0 1.73-3" />
      <path d="M12 9v4" />
      <path d="M12 17h.01" />
    </>
  ),
  "user-cog": () => (
    <>
      <path d="M10 15H6a4 4 0 0 0-4 4v2" />
      <path d="m14.305 16.53.923-.382" />
      <path d="m15.228 13.852-.923-.383" />
      <path d="m16.852 12.228-.383-.923" />
      <path d="m16.852 17.772-.383.924" />
      <path d="m19.148 12.228.383-.923" />
      <path d="m19.53 18.696-.382-.924" />
      <path d="m20.772 13.852.924-.383" />
      <path d="m20.772 16.148.924.383" />
      <circle cx="18" cy="15" r="3" />
      <circle cx="9" cy="7" r="4" />
    </>
  ),
  "users": () => (
    <>
      <path d="M16 21v-2a4 4 0 0 0-4-4H6a4 4 0 0 0-4 4v2" />
      <path d="M16 3.128a4 4 0 0 1 0 7.744" />
      <path d="M22 21v-2a4 4 0 0 0-3-3.87" />
      <circle cx="9" cy="7" r="4" />
    </>
  ),
  "webhook": () => (
    <>
      <path d="M18 16.98h-5.99c-1.1 0-1.95.94-2.48 1.9A4 4 0 0 1 2 17c.01-.7.2-1.4.57-2" />
      <path d="m6 17 3.13-5.78c.53-.97.1-2.18-.5-3.1a4 4 0 1 1 6.89-4.06" />
      <path d="m12 6 3.13 5.73C15.66 12.7 16.9 13 18 13a4 4 0 0 1 0 8" />
    </>
  ),
};

/**
 * One glyph, sized to the text it accompanies.
 *
 * Always `aria-hidden`. Every icon in this console rides beside a visible label — a nav entry, a
 * group heading, a palette row — so the label is the accessible name and announcing the icon as
 * well would read the same thing twice. An icon that ever stood alone would need its own label and
 * would be a different component: this one is decoration by construction.
 */
export function Icon(props: { name: IconName; class?: string }) {
  return (
    <svg
      aria-hidden="true"
      viewBox="0 0 24 24"
      fill="none"
      stroke="currentColor"
      stroke-width="2"
      stroke-linecap="round"
      stroke-linejoin="round"
      class={props.class ?? "h-4 w-4 shrink-0"}
    >
      {GLYPHS[props.name]()}
    </svg>
  );
}
