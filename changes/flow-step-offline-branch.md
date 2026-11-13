---
kind: added
upgrade_note: Protocol, additive. A flow step may carry `offline` ({"skip_note_key": …} or {"step_id": …}); absent by default and left off the wire. The cloud now refuses at publish a required step whose act is ACTION_EFFECT_EXTERNAL without one (NO_OFFLINE_BRANCH), a malformed branch (BAD_OFFLINE_BRANCH), and a note key without English text. No published version is affected: the only such act, integrations.lookup.run, is new in this release.
---
- **A flow step that needs the cloud says what happens offline.** A step may skip itself with a
  note, or go to another step of its flow, when the store cannot reach the cloud. A required
  lookup step without one is refused at publish, and the console's step editor offers the choice
  (ADR-0214 decision 5).
