---
kind: changed
upgrade_note: A package version whose translation keys lack an English text in the tenant's grid was published before and is now refused (`400`, `UNKNOWN_TRANSLATION_KEY`). Add the English text under Translations before publishing; versions already published are unchanged.
---
- **Every label a package version names needs English before it publishes.** A time slot's name, a
  screen's label or title, and a custom field's, list's or object type's label are refused at
  publish, and named by the designer's check, when the tenant's translations have no English text
  for them, so a till never draws an empty label (ADR-0190 decision 6).
