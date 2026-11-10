# ADR-0209 — The console previews a screen with the till's own renderer, shared from `packages/shared`

**Status** Proposed · **Owner** @maintainers-architecture · **Date** 2026-11-10
· Amends [ADR-0170](0170-repository-layout-code-naming-and-size-rules.md) decision 1 (what
`packages/shared` may hold) · Relates to [ADR-0190](0190-a-screen-is-drawn-from-the-package.md),
[ADR-0203](0203-a-component-reads-its-own-data.md), [ADR-0208](0208-a-tap-is-measured-to-its-frame-ready-and-gated-per-pull-request.md)

Waits on the owner's choice among the options below.

## Context

Phase 4 ([roadmap v4](../roadmap-v4.md)) asks for a live preview in the console "through the till's
own renderer", proven to match the till. The renderer is `ui/src/package/render.tsx` with its
catalogue table `registry.tsx`. Three facts constrain where the console can get it from:

- **The console builds alone.** The cloud image and the `dashboard` CI job build from
  `dashboard/` and `packages/shared/` only (`deploy/Dockerfile`, `pr.yml`), so the console cannot
  import `ui/src`.
- **`packages/shared` imports no package** (its README, ADR-0170 decision 1). A renderer imports
  `solid-js`.
- **Most components are till regions.** `order_bill`, `kds_tickets` and their kind draw what a till
  screen hands over through `RegionsContext`, with its state and its edge calls. Only `stack`,
  `text`, `action_button`, `screen_frame` and `page_header` draw from the package alone.

## Options considered

1. **Move the renderer's core into `packages/shared/package/`** (recommended). The tree walk, the
   limits, the fallback, the contexts, variant resolution, the generated component and action
   catalogues (`pos-proto` writes them there), and the components that draw from the package alone.
   `solid-js` is the only import, resolved from the front end that builds the file (`resolve.dedupe`
   and a `paths` entry in each front end's config). The till keeps its region components and its
   label lookup and hands both to the shared renderer. The console's preview hands it the same
   label lookup over the tenant's translation grid, and draws each region as a labelled outline of
   the area the till fills.
2. **Serve the till's build from the cloud** and preview in a frame. The till's own bundle, with its
   regions. But the cloud image grows a second front end, a preview mode lives in the till, and the
   console talks to the frame by messages.
3. **Let the console import `ui/src`.** The image and the `dashboard` job would need `ui/` on disk,
   and the console would pull in the till's screens, state and edge client to draw five components.

## Decision

Option 1:

1. `packages/shared/package/` holds the till's renderer core and the generated catalogues. It may
   import `solid-js`, and nothing else. Neither front end keeps a copy (`xtask mirrored-files`).
2. A region is the host's. The till supplies its regions as now. The preview supplies an outline
   that names the component, sized as the till's layout sizes it.
3. **"The preview matches the till" is a test.** A fixture package, the default package and a
   designed screen, is rendered by the till's mount and by the console's preview. Each writes its
   DOM, with region contents left out, and the two must be equal. Both front ends' tests check one
   committed fixture.
4. Moving the core is a till change under ADR-0208. Its pull request carries the perf gate at 2×,
   branch against main, and the full till replay.

## Consequences accepted

- `packages/shared` holds TSX that `solid-js` must resolve, and its README says so. Both front ends
  must agree on a `solid-js` major version.
- `pos-proto`'s TypeScript generators write to `packages/shared/package/`. That is a path change
  in `pos-proto`, with the owner's review, and no change to the protocol.
- The preview cannot show what a region holds on a live store (the order's lines, the tickets).
  It shows where each region sits.
