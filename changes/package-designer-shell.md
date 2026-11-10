---
kind: added
upgrade_note: The package version publish (`POST /admin/packages/{package_id}/versions`) now takes `screens`, checked as the edge checks them, and a version's minimum edge release counts each component and act its screens name. The new `POST /admin/packages/{package_id}/versions/dry-run` needs `console.packages.manage`.
---
- **HQ designs a package version in the console.** *Design a version*, under Packages, opens a
  package's newest version as a draft, kept in the browser while it is edited; *Check* runs the
  cloud's own publish checks without storing anything, and each refusal is shown beside what it
  refuses; *Publish* publishes only a draft the cloud accepts. A version's screens can now be
  published.
