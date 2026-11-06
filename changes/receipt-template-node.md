---
kind: added
upgrade_note: A new setting, `receipt.templates` on a new `receipt` node, with the kind `SETTING_KIND_RECEIPT_TEMPLATES`; its default is no template, so every store prints as before. `settings_router` takes the tenant's translation store, and `CountryModule` gains `receipt_required_sections`, empty by default.
---
- **A receipt template per store and sales channel can be written.** `receipt.templates` holds a
  base template and one per sales channel: header and footer lines from the translation grid, the
  optional sections it leaves out, the custom fields it prints and which documents are the person's
  own copy. The cloud refuses free text, a key with no English, a value over its limits and a
  section the store's country requires (ADR-0207).
