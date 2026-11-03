---
kind: added
---
- **ADR-0194, 0198 and 0199, accepted: custom data, the cloud gateway and WebAssembly functions.**
  HQ declares custom fields, object types and lists in a package; every field says whether it is
  personal, and a personal value is kept in the subject store with a retention period and a
  recorded consent, never in an event or a log. A store reaches the internet only through the cloud,
  behind the webhook guard, with deadlines, idempotency and rate limits. Custom code, once a real
  case needs it, is reviewed, signed, pure WebAssembly that returns a decision and never touches
  money. The code follows in its own changes.
