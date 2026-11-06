---
kind: changed
---
- **The floor is drawn from the default package.** The till's screens become a package one at a
  time (ADR-0190): the floor plan is now a tree of catalogued components (`screen_frame`,
  `page_header`, `floor_alerts`, `floor_plan`) in `ui/src/package/default-package.json`, which the
  same check a published version passes holds. It looks and works exactly as before.
