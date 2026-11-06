---
kind: added
---
- **The till runs its package's reactions.** A package version can now make the till act by itself
  when something happens: a table seated, a dish added, a bill opened, or a table's order screen
  shown, and only when the version's condition holds. A reaction opens a screen or prints a pre-bill
  or an X report, after the screen has moved, never in the way of a tap, at most 16 at a time, and
  only what the signed-in person may do without a manager. The store server's
  `GET /api/package` now carries the running version's reactions for the till (additive).
