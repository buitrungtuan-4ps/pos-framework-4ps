# ADR-0211 — An area may sit inside another area, and the cloud publishes the tree resolved

**Status** Accepted · **Owner** @maintainers-architecture · **Date** 2026-11-12
· Extends [ADR-0072](0072-floor-and-kitchen.md) (what the floor plan holds)
· Relates to [roadmap v4](../roadmap-v4.md) phase 4, roadmap-v3 B8.1 to B8.3,
[ADR-0208](0208-a-tap-is-measured-to-its-frame-ready-and-gated-per-pull-request.md)

The owner chose the tree on 2026-11-12, stored as a parent pointer and published resolved.

## Context

Roadmap v4 phase 4 names "the multi-floor drag-and-drop floor plan". A store's plan today is a flat
list of areas, each with tables at an optional `GridPosition`, published as the `floor` node
(`pos_proto::floor::FloorPlan`). A restaurant on several storeys, or with rooms inside a section,
shows every area on one long screen, and a server upstairs scrolls past the ground floor.

The owner asked for the most flexible grouping that stays safe: floors, sections within floors,
rooms within sections, without a fixed number of levels.

## Options considered

1. **A floor entity and a `floor_id` on each area.** Simple, but exactly one level of grouping.
2. **A free-text floor tag on areas.** No entity: two spellings make two floors.
3. **Areas inside areas, stored as a parent pointer** (chosen). One type for every level. Moving a
   branch is one write.
4. **A stored path or a closure table.** Faster subtree reads, but every move rewrites the branch,
   and a plan of tens of areas gains nothing from it.
5. **Nested sets.** Fast reads, and every edit renumbers the tree. Wrong for data HQ edits often.
6. **Labels (tags) on areas.** Groups that cross the tree (outdoor, smoking, a server's section).
   They do not say what is inside what; they are left for later and can be added beside the tree.

## Decision

1. **An area may name a parent area** (`parent_area_id`, optional). An area without one is at the
   top. A floor is simply a top-level area; nothing in the model is called a floor.
2. **The tree is bounded.** The cloud refuses a write that would make a cycle, or a branch deeper
   than **8** levels (AGENTS.md: every structure has an explicit limit). A plan deeper than **3**
   levels is a warning in the console, never a refusal. Areas are archived, never deleted, and an
   area with live children cannot be archived until they move.
3. **Tables sit on any area**, at any level, as today.
4. **The cloud publishes the tree resolved.** The `floor` node keeps its flat list of areas, in tree
   order, each carrying additively its `parent_area_id`, its `depth` and its `path` (the area ids from
   the top). The edge and the till never walk the tree: they read the order and the path the cloud
   computed. A till that predates the fields ignores them and draws the areas as today.
5. **The till** shows the top-level areas as tabs once there is more than one, and the levels below
   as a breadcrumb (`Tầng 2 › VIP › Phòng 1`) with their tables. A store whose areas are all at the
   top draws exactly what it draws today.
6. **Reports** stay per area that holds the table, and roll up along the published path.
7. **Nothing about money, events or personal data changes.** An order names its table, never its
   area's parents.

## Consequences accepted

- **Additions to `pos-proto`:** `FloorArea.parent_area_id`, `FloorArea.depth`, `FloorArea.path`. All
  additive, with the owner's review. The `floor` node's snapshot and the edge's OpenAPI grow.
- **A migration** adds a nullable `parent_area_id` to areas, with the cycle and depth checks in the
  cloud's write path, and `/admin/floor/areas` takes the parent.
- **The console's floor plan** shows the tree, moves a branch by drag and drop, and warns past three
  levels.
- **The till's floor screen gains tabs and a breadcrumb**, a till change under ADR-0208's perf gate.
