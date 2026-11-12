---
kind: changed
---
- **The till's package renderer lives in `packages/package-renderer/`, where the console can build
  it too** (ADR-0209). The till draws its screens exactly as before.
  - The core moved there: the tree walk, the limits, the fallback, the contexts, variant
    resolution, and the five components that draw from the package alone (`stack`, `text`,
    `action_button`, `screen_frame`, `page_header`). The till keeps its regions and its words.
  - `pos-proto` writes the component and action catalogues there, once, instead of one copy in
    `ui/` and one in `dashboard/`. The protocol did not change.
  - Each front end resolves `solid-js` for it from its own root (`resolve.dedupe`, a `tsconfig.json`
    `paths` entry). The cloud image and the `dashboard` CI job copy the directory beside
    `packages/shared/`, and `xtask mirrored-files` fails on a copy in either front end.
