---
kind: fixed
---
- **Shared settings says a value came from the country pack.** A value the store's country pack
  wrote, through the new-store values or the store's country on Store settings, reads *From the
  Vietnam pack.* (en/vi) instead of *Set here on …*. The stored value records the pack's country as
  `pack_country_code`, an optional field on `GET /admin/settings` and `PUT /admin/settings`, kept
  only when the pack gives the setting that value; a person's save clears it. Values written before
  this release show as before.
