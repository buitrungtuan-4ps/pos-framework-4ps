---
kind: fixed
---
- **A device that has just paired signs in in the store's language.** `GET /api/session`, which a
  paired device reads before anyone signs in, now carries the store's `display_language` (the only
  `locale` fact it carries, and absent where the store sets none), and the sign-in screen adopts it.
  A language the operator chose on the device still wins. Until now a device nobody had signed in
  on showed its first sign-in in English, because the language came only from `GET /api/locale`,
  which answers a signed-in person. A device not yet paired still starts in English.
