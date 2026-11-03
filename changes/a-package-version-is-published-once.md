---
kind: added
---
- **HQ can create a package and publish versions of it** (ADR-0189). `POST /admin/packages`
  creates a package, and `POST /admin/packages/{package_id}/versions` publishes a version: its
  label and its named time slots, with an id and a minimum edge release the server sets. A
  published version is stored once and never changes. Both need the new console permission
  `console.packages.manage` (Owner and Admin); the lists need `console.data.read`. No store runs a
  version yet: pinning one arrives with the package settings.
