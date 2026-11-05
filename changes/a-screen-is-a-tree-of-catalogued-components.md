---
kind: added
upgrade_note: A package version's content may now carry `screens`, an optional list left off the wire when empty; an edge on an older release ignores it and draws its own screens.
---
- **A package screen is a tree of catalogued components.** `pos-proto` gains `screen`: a `Screen`
  is a `screen_id` and a tree of nodes, each naming a component from the release's catalogue and
  giving it literal properties of five kinds (translation key, token, whole number, flag, or an act
  with fixed parameters), at most 400 nodes and 12 levels (ADR-0190, ADR-0203). The first catalogue
  has `stack`, `text` and `action_button`; each component states what it reads, what it supplies to
  the acts it binds, the device classes it is drawn on, its fallback and its release.
  `docs/snapshots/components.txt` refuses a removed component, property or token, and the till's
  view is `ui/src/package/components.ts`. A version's content gains `screens`, with variants.
  Nothing draws them yet, so no till behaves differently.
