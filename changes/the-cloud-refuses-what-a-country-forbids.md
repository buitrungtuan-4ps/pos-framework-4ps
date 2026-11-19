---
kind: added
upgrade_note: "A new settings refusal, `SETTING_FORBIDDEN_BY_COUNTRY` (`400` on `value`): `PUT /admin/settings` refuses a value a country pack's `forbidden_settings` names when any store the write reaches is in that country; `vn`, `jp` and `in` forbid `weighing.manual_entry` on. A store with no published country does not block a write. The catalogue item's `codes` may carry `sold_by` and `tare_grams`, which the console edits."
---
- **The console sells an item by weight, and refuses what a country's law forbids** (ADR-0223).
  An item's drawer marks it sold by weight, with an optional tare in grams. Turning on hand-typed
  weights for a store in Vietnam, Japan or India, or for a scope that reaches one, is refused and
  explained, since their law takes a weight from a certified scale only.
