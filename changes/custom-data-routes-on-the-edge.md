---
kind: added
---
- **The till's edge serves custom data.** `POST /api/custom-fields` sets a package's custom fields
  on an order, line, bill, table or shift, and `POST /api/custom-records` and
  `POST /api/custom-records/{id}/values` create and change records of its object types, under
  `data.record.write` and the object's own permission; each has a `GET` beside it. No answer
  carries a personal value. `GET /api/subjects/{subject_id}` shows one subject's personal values to
  a person holding `data.personal.read`, and every read is logged by actor, subject and action,
  never by value (ADR-0206). Three acts join the catalogue: `data.custom_field.set`,
  `data.custom_record.create` and `data.custom_record.change`.
