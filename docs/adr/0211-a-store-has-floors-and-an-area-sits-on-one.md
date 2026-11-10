# ADR-0211 — A store has floors, and an area sits on one

**Status** Proposed · **Owner** @maintainers-architecture · **Date** 2026-11-10
· Extends [ADR-0072](0072-floor-and-kitchen.md) (what the floor plan holds)
· Relates to [roadmap v4](../roadmap-v4.md) phase 4, roadmap-v3 B8.1 to B8.3

Waits on the owner's choice among the options below.

## Context

Roadmap v4 phase 4 names "the multi-floor drag-and-drop floor plan". A store's plan today is areas,
each with tables at an optional `GridPosition`, published as the `floor` node
(`pos_proto::floor::FloorPlan`). The console drags tables onto each area's grid, and the till draws
the areas as sections. Nothing says which areas are upstairs. A restaurant on two or three storeys
shows every area on one long screen, and a server upstairs scrolls past the ground floor to reach
their tables.

## Options considered

1. **A floor entity, and an optional `floor_id` on each area** (recommended).
   - A store has floors `{floor_id, name, position}`. An area names the floor it is on.
   - `FloorPlan` gains `floors`, additively. An area with no `floor_id` sits on the store's only
     floor, so every plan published today means what it meant.
   - The console groups the areas by floor, with a tab per floor and drag-and-drop within each. The
     till's floor screen shows a tab per floor once the store has more than one.
2. **Floors as a tag on areas only** (a free-text `floor_name`). No entity, no order, no rename in
   one place. Two spellings of "Tầng 2" make two floors.
3. **Floors as areas of areas** (a nested area tree). One type covers both, but the till's sections,
   the QR sheet and every report that groups by area would have to learn a tree for one level.

## Decision

Option 1:

1. **Floors are store-scoped master data**, authored in the console like areas (ADR-0072). Each has
   a `floor_id` (a new `FloorId` ULID), a name and a position. Floors are archived, never deleted.
2. **An area may name its floor.** An area without one is on the first floor. Moving an area to
   another floor is one write.
3. **The `floor` node carries the floors**, in position order, beside the areas. It is additive. A
   till that predates the field ignores it and draws the areas as today.
4. **The till shows one floor at a time** when the store has more than one: a tab per floor on the
   floor screen, remembered per device. A table's order screen and every act stay as they are.
5. **Nothing about money, events or personal data changes.** Floors are plan data. An order names
   its table, never its floor.

## Consequences accepted

- **Additions to `pos-proto`:** `FloorId`, `FloorPlan.floors` and `FloorArea.floor_id`. All are
  additive and carry the owner's review. The `floor` node's snapshot and the edge's OpenAPI grow.
- **A migration** adds the floors table and a nullable `floor_id` on areas, with RLS like areas.
  Admin routes for floors are added under `/admin/floor/floors`, behind `console.floor.manage`.
- **The till's floor screen gains tabs**, a till change under ADR-0208's perf gate. A store with one
  floor draws exactly what it draws today.
