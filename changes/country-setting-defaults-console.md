---
kind: added
upgrade_note: The console's `GET /admin/countries` gains `setting_defaults`, each a `setting_key` and a `value`, the settings the country's pack gives a new store (ADR-0183); additive, so a client that ignores it is unaffected. Store settings writes them only when it publishes a country the store did not have, so a Vietnamese store that published its country before this release still needs its new-store values given again from Shared settings.
---
- **Publishing a store's country gives it the country pack's settings.** When Store settings
  publishes a locale that names a country the store did not have, the console writes each setting
  the pack gives that no scope sets, at the store's own scope, keeps any the tenant, a brand, a
  group or the store sets otherwise, and says which. A store made with the new-store wizard, which
  runs before it has a country, so ends with Vietnam's 20,000 ₫ over/short threshold once its
  country is published, with no extra step. `GET /admin/countries` lists each pack's settings as
  `setting_defaults` (ADR-0183).
