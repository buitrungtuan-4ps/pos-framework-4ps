---
kind: added
upgrade_note: Migration 0092 adds the tables `custom_values` and `custom_value_cursors`, each with row-level security by tenant and readable by `app_tenant`. It is additive and applied on boot. The projector fills them from each store's log on its next ticks, starting at the beginning of the log.
---
- **The cloud keeps the current value of every custom field** (ADR-0194). The rollup projector now
  also folds each store's `data.custom_field.set` and `data.custom_record.changed` events into one
  row per object and field. Only non-personal values and the `subject_id` of an object's personal
  record are kept; no personal value reaches the cloud.
