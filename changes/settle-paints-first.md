---
kind: changed
---
- **Taking payment paints at once, and waits for the store server only to finish.** A tender tap
  shows the payment pending straight away, with the amount due and no change or receipt until the
  store server commits; the drawer and the paper still wait for it. What the till can tell itself
  (a write still on its way, cash into a counted drawer, payments that do not add up) is refused
  before anything is sent. A refusal brings the pad back with its sentence in place, to be
  dismissed. A payment the server does not answer is checked, never taken back blind, and a manager
  can leave it to be looked up later (ADR-0196, ADR-0204).
