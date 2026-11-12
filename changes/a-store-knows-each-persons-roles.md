---
kind: added
upgrade_note: Protocol, additive. The `permissions` node's staff members gain `role_ids` and `GET /api/session` answers `role_ids` (ADR-0213). A member with no role is published byte-identical, and an edge that predates the field ignores it. A package version a selector of which states a role now needs at least `ROLE_VARIANTS_SINCE` (0.14.1).
---
- **A till shows each person their role's screens and home.** The cloud publishes the roles each
  member of staff holds at the store, the store answers the signed-in person's roles, and the till
  draws a screen's role variant and opens a role's home after sign-in. When two of a person's roles
  each have a variant, the one listed first in the version wins; with none, the base, as before.
  Role ids are never written to an event or a log.
