# Noto Sans, self-hosted

The console's typeface (Wave 4 · PR-2, decision D2). Six variable `woff2` subsets of **Noto Sans**,
served from this directory by the same origin as the console.

| File | Subset | Bytes |
| --- | --- | --- |
| `noto-sans-latin.woff2` | Latin | 35,820 |
| `noto-sans-latin-ext.woff2` | Latin Extended | 167,960 |
| `noto-sans-vietnamese.woff2` | Vietnamese | 14,456 |
| `noto-sans-cyrillic.woff2` | Cyrillic | 20,080 |
| `noto-sans-cyrillic-ext.woff2` | Cyrillic Extended | 70,680 |
| `noto-sans-greek.woff2` | Greek | 21,776 |

**What a reader downloads is not that total.** Each `@font-face` in `src/styles/app.css` carries a
`unicode-range`, so the browser fetches only the subsets the page actually paints: 36 kB for an
English console, 50 kB for a Vietnamese one. The rest sit here for a fork whose market needs them.

Each file is the **variable** font — one file covers every weight — so the faces declare
`font-weight: 100 900` rather than shipping a file per weight. Downloading the three weights
separately returns three byte-identical files; the earlier estimate of ~150 kB for "three weights"
was wrong in both directions, and this table is the measurement.

Scripts beyond these six ship with the country module that needs them (Thai, Devanagari, Arabic),
which is what keeps a framework that already has VN, JP and IN modules from bundling several
megabytes it cannot use. CJK is not bundled at all and falls back to the system font.

## Provenance and licence

Fetched from Google Fonts (`fonts.googleapis.com/css2?family=Noto+Sans`, family version v42) on
2026-09-15. Noto Sans is licensed under the **SIL Open Font License 1.1** — the full text is in
[`OFL.txt`](OFL.txt) beside these files, which is what the licence requires of anyone redistributing
the font.

## Refreshing

Re-fetch the CSS with a modern browser `User-Agent` (Google serves `woff2` only to browsers that
support it), take one URL per subset, and save it under the name in the table above. Then re-run
`pnpm build` — `scripts/asset-weight.mjs` fails if any single file grows past its ceiling.
