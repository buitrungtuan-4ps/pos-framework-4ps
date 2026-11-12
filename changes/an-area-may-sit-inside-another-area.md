---
kind: added
upgrade_note: Migration 0096 adds a nullable parent_area_id to floor_areas; every existing area reads as top-level and nothing is backfilled. The floor node's FloorArea gains parent_area_id, depth and path, all additive and left off the wire for a top-level area, so PROTOCOL_VERSION is unchanged and a store whose areas are all at the top publishes the same node as before.
---
- **An area may sit inside another area.** The console's `/admin/floor/areas` takes a
  `parent_area_id`, and the cloud refuses a cycle, a branch deeper than eight levels, a parent that
  is archived or not the store's, and archiving an area whose children are still live. The `floor`
  node is published in tree order with each area's parent, depth and path resolved (ADR-0211).
