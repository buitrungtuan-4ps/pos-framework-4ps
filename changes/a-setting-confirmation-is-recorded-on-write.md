---
kind: added
---
- **`PUT /admin/settings` takes `notice_confirmed`.** Writing a value that its setting's catalogue
  entry names in `confirm`, at any scope, is refused with `400` (`notice_confirmed` /
  `CONFIRMATION_REQUIRED`) unless the request carries `notice_confirmed: true` (ADR-0169). The
  `setting.set` audit entry then records `notice_confirmed: true` and the notice's translation key
  beside the value. Any other value, and clearing a setting, needs no confirmation. The field is
  optional and defaults to `false`; no setting uses it yet.
