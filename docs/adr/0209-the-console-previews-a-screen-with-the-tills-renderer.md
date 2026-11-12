# ADR-0209 — The console previews a screen with the till's own renderer, shared as its own package

**Status** Accepted · **Owner** @maintainers-architecture · **Date** 2026-11-12
· Relates to [ADR-0170](0170-repository-layout-code-naming-and-size-rules.md),
[ADR-0189](0189-a-store-runs-a-package-version-hq-pins.md),
[ADR-0190](0190-a-screen-is-drawn-from-the-package.md),
[ADR-0203](0203-a-component-reads-its-own-data.md),
[ADR-0208](0208-a-tap-is-measured-to-its-frame-ready-and-gated-per-pull-request.md)

The owner chose option 4 on 2026-11-12.

## Context

Phase 4 ([roadmap v4](../roadmap-v4.md)) asks for a live preview in the console "through the till's
own renderer", proven to match the till. The renderer is `ui/src/package/render.tsx` with its
catalogue table `registry.tsx`. Three facts constrain where the console can get it from:

- **The console builds alone.** The cloud image and the `dashboard` CI job copy `dashboard/` and
  `packages/shared/` only (`deploy/Dockerfile`, `pr.yml`), so the console cannot import `ui/src`.
- **`packages/shared` imports no package** (its README, ADR-0170 decision 1). A renderer imports
  `solid-js`.
- **Most components are till regions.** `order_bill`, `kds_tickets` and their kind draw what a till
  screen hands over through `RegionsContext`, with its state and its edge calls. Only `stack`,
  `text`, `action_button`, `screen_frame` and `page_header` draw from the package alone.

## Options considered

1. **Move the renderer's core into `packages/shared/package/`.** One renderer and a provable match,
   but `packages/shared` would have to give up its rule of importing nothing.
2. **Serve the till's build from the cloud** and preview it in a frame. Closest to the till, but the
   cloud image grows a second front end, the till grows a preview mode, and the two talk by messages.
3. **Let the console import `ui/src`.** Breaks the console's lone build and drags in the till's
   screens, state and edge client to draw five components.
4. **A package of its own, `packages/package-renderer/`** (chosen). The same core as option 1, in a
   directory whose one job is rendering a package, so `packages/shared` keeps its rule.
5. **Preview per edge release**: each till release ships its renderer as a module the cloud keeps,
   and the console loads the one a store runs. Exact per release, but it needs stored bundles and
   dynamically loaded code under a strict content security policy. Left for when stores run
   several releases at once; option 4 does not stand in its way.

## Decision

1. **`packages/package-renderer/` holds the till's renderer core**: the tree walk, the limits, the
   fallback, the contexts, variant resolution, the generated component and action catalogues
   (`pos-proto` writes them there), and the components that draw from the package alone. It imports
   `solid-js` and `packages/shared`, nothing else, resolved from the front end that builds it
   (`resolve.dedupe` and a `paths` entry in each front end's config). Neither front end keeps a copy
   (`xtask mirrored-files`). `packages/shared` is unchanged.
2. **The console builds it alone too.** `deploy/Dockerfile` and the `dashboard` job copy
   `packages/package-renderer/` beside `packages/shared/`, and `xtask impact` maps the directory to
   both front ends.
3. **A region is the host's.** The till supplies its regions as now. The preview supplies an outline
   that names the component, sized as the till's layout sizes it.
4. **"The preview matches the till" is a test.** A fixture package, the default package and a
   designed screen, is rendered by the till's mount and by the console's preview. Each writes its DOM,
   with region contents left out, and the two must be equal. Both front ends' tests check one
   committed fixture.
5. **What can be previewed** is any version, for any role and device class its variants name, the
   same versions HQ pins at tenant, brand, group or store scope (ADR-0189).
6. Moving the core is a till change under ADR-0208. Its pull request carries the perf gate at 2×,
   branch against main, and the full till replay.

## Consequences accepted

- A new top-level package directory, with its own README, and both front ends must agree on a
  `solid-js` major version.
- `pos-proto`'s TypeScript generators write to `packages/package-renderer/`: a path change in
  `pos-proto`, with the owner's review, and no change to the protocol.
- The preview cannot show what a region holds on a live store (the order's lines, the tickets). It
  shows where each region sits, and it draws with the current release's renderer, not an older one a
  store may still run (option 5).
