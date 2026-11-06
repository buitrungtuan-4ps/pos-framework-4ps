---
kind: added
upgrade_note: Two permissions join the catalogue, data.record.write (LOW; servers, cashiers, supervisors, managers and owners by default) and data.personal.read (HIGH; managers and owners by default). A role defined in the cloud before this release does not hold them until an owner grants them.
---
- **The edge enters custom data.** It sets a package's custom fields on an order, line, bill,
  table or shift, and creates and changes records of its object types, after checking each value
  against the package's declarations. Non-personal values go into `data.custom_field.set` and
  `data.custom_record.changed`; a personal value goes only to the store's subject store, with the
  time consent was given, and expires on its field's own retention period (ADR-0206). No till
  screen or route uses it yet.
