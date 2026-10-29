---
kind: added
upgrade_note: The new-store values a store is given (`POST /admin/settings/presets`) now take the store's country pack's value before the register's preset (ADR-0183). A store whose published `locale` names Vietnam is given `shift.variance_reason_minor = 20000` (đồng), so its drawer closes ask a reason past 20,000 ₫ over or short; a store elsewhere, or with no published locale, is given exactly what it was before. Only a setting the store does not already set at its own scope is written, as before. The console's new-store wizard runs before a store has a country, so for an existing or newly created Vietnamese store, give its new-store values again from Shared settings once its country is published. A fork with its own `CountryModule` changes nothing: `setting_defaults()` has a default that seeds none.
---
- **A country pack seeds a new store's settings.** `CountryModule` gains `setting_defaults()`, a
  list of setting keys and values in the country's units, empty by default. Applying a store's
  new-store values takes the store's country pack's value first, then the register's preset, and
  still writes only what the store does not set. The Vietnam pack seeds
  `shift.variance_reason_minor = 20000`, the owner's figure (ADR-0183). A test over every
  compiled-in pack checks that each key is in the register and each value one its setting takes.
