---
kind: added
upgrade_note: The settings register gains the `package` node's five settings and two value kinds, `SETTING_KIND_ULID` and `SETTING_KIND_ULID_LIST`, and the store permission catalogue gains `admin.package.choose` (default roles Manager and Owner, not yet granted to existing roles, because no route checks it yet). Nothing changes for a store until HQ pins a version.
---
- **HQ pins a store to a package version** (ADR-0189). The `package` settings lock a store to one
  version, or let it choose among versions HQ allows. They are written at the tenant, a brand, a
  store group or a store, and may name only published versions. A lock cannot be loosened at a
  narrower scope (`409 PACKAGE_LOCKED_ABOVE`). A store's choice is written only through
  `PUT /admin/stores/{store_id}/package/choice`, and only from its allowed versions. For now HQ
  makes the choice for the store, with `console.config.publish`. The console's Settings page leaves
  these settings out; the package page will set them.
