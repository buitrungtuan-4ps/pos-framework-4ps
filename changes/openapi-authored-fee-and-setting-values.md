---
kind: changed
---
- **A fee rule as written, a fee's faults and a setting's value have schemas.** The fee list's and
  effective fees' `rule` is an `AuthoredFeeRule` (the wire rule's fields and the item categories
  it names), a fee fault's `reason` lists its tokens, and a setting's `value`, `default` and
  `preset` are one of a token, a whole number or a flag (`SettingValueSchema`) in
  `docs/openapi-admin.json`. The bytes on the wire are unchanged.
