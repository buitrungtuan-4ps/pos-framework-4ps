---
kind: added
upgrade_note: Additive throughout. The event catalogue gains `data.subject.masked` (schema version 1), a store's configuration may carry a new `erasures` node (at most 1,000 subject ids), and store-postgres migration 0093 adds the `custom_subjects` table, folded by the projector behind the existing custom-value cursor. A cloud upgraded over a log that already holds custom data does not refold it, so `custom_subjects` starts from the events after the upgrade; until this release the cloud refused every personal field at publish, so no earlier event names a subject of custom data. `POST /admin/subjects/{subject_id}/erase` gains `pending_store_ids` and may answer `erased: false`; `GET /admin/subjects/{subject_id}/stores` is new. An edge that predates this release ignores the node. Rollback is safe: the table and the node are left unread.
---
- **An erasure reaches the store that holds the subject** (ADR-0206 decision 4). A personal
  value entered at a till stays on that store. When the owner erases a subject the cloud does not
  hold, the cloud now adds its id to the `erasures` node of each store whose events name it. The
  store masks the subject on its next configuration pull and confirms with `data.subject.masked`,
  which carries only the id and the time. The cloud then takes the id off the node. The console's
  *Subject requests* screen lists the stores that hold such a subject and shows the erasure as
  pending until each store confirms.
