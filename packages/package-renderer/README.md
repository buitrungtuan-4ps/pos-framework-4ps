# `packages/package-renderer` — the renderer of a package screen

**Owner** @maintainers-architecture · [ADR-0209](../../docs/adr/0209-the-console-previews-a-screen-with-the-tills-renderer.md)

The till (`ui/`) draws a package's screens with this renderer, and the console (`dashboard/`)
previews them with the same code, so the preview is the till's drawing and not a copy of it.

| File | What it is |
|---|---|
| `src/render.tsx` | The tree walk, the node and depth limits, the fallback for a component this release lacks, and the contexts: the host screen (`ScreenHost`) and the front end's words (`Words`) |
| `src/base.tsx` | The components that draw from the package alone: `stack`, `text`, `action_button`, `screen_frame`, `page_header` (`BASE`) |
| `src/regions.tsx` | `RegionsContext` and `region(id)`: a region is drawn by the host, never here |
| `src/variant.ts` | Which variant of a package part a device draws (ADR-0189 decision 4), tested against `pos-core`'s shared vectors |
| `src/shape.tsx` | `marked` and `shape`: what a drawn tree's DOM is, region contents left out, for the test that the preview matches the till |
| `fixtures/` | That test's one fixture (`preview-package.json`, the default package and a designed screen) and the shape both front ends must draw (`preview-shape.txt`) |
| `src/components.ts`, `src/actions.ts` | The component and action catalogues, written by `pos-proto` (`POS_UPDATE_SNAPSHOTS=1 cargo test -p pos-proto snapshot`); never edit them by hand |

## Rules

- **It imports `solid-js` and nothing else** (and `packages/shared`, if it ever needs it). It has no
  `node_modules`: each front end resolves `solid-js` from its own root (`resolve.dedupe` in its Vite
  and Vitest configs, a `paths` entry in its `tsconfig.json`), so there is one Solid in each build.
- **No words, no data, no edge.** A front end hands its words in (`Words`), the host screen its
  subject and its acts (`ScreenHost`), and the regions their contents.
- **No copies.** `cargo xtask mirrored-files` fails if a file here goes missing, or a copy appears
  in `ui/` or `dashboard/`.
- **Each front end completes the table.** The till's `ui/src/package/registry.tsx` and the console's
  preview are each `{ ...BASE, …regions }`, a `Record` over the catalogue, so a component catalogued
  without an implementation does not compile in either.
- **Tailwind scans it** from each front end (`@source` in `src/app.css`).
- **The cloud image copies it** beside `dashboard/` (`deploy/Dockerfile`), and the `dashboard` CI
  job builds with only `dashboard/` and the two `packages/` directories on disk.

## How to test

From `ui/`: `pnpm test` (the renderer and variant tests, `unit/package-*.test.ts`, and
`unit/preview-matches.test.ts`). From `dashboard/`: `pnpm test` (`tests/preview-matches.test.ts`).
When the till's drawing changes on purpose, `pnpm exec vitest run -u unit/preview-matches.test.ts`
in `ui/` rewrites `fixtures/preview-shape.txt`, and the console's test must then agree with it. The
fixture's first screens are the default package's, and the till's test fails when they differ. A change here runs both front ends' jobs (`xtask affected`, class
`shared`).
