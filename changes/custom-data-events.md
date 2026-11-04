---
kind: added
upgrade_note: The event catalogue gains `data.custom_field.set` and `data.custom_record.changed` (schema version 1). They are additive. No edge writes them yet, and a consumer that does not know them keeps them as unrecognised events.
---
- **Two events for custom data** (ADR-0194). `data.custom_field.set` records values set on an
  order, line, bill, table or shift. `data.custom_record.changed` records a record of a package's
  object type. Each value is an integer, a yes or no, a list entry, a date or a quantity. A
  personal value never travels in an event: the event carries only the `subject_id` of the record
  that holds it.
