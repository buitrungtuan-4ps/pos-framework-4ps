---
kind: added
---
- **The settings register can mark a value that needs a recorded confirmation.** A setting may
  name one value that is written only when the person writing it confirms a notice (ADR-0169). The
  register checks that such a value is never a setting's default or a new store's preset,
  `docs/snapshots/settings.txt` carries it as a `confirm=` line that may not be removed, and
  `GET /admin/settings/catalogue` serves it as `confirm`, with its notice's translation key. No
  setting uses it yet.
