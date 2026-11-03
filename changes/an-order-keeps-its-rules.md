---
kind: changed
upgrade_note: `sales.order.opened` now carries `package_version_id` whenever the store runs a pinned package version. The field already existed in the schema and is optional; an order opened under the default package still writes the same bytes. A consumer that ignores unknown fields needs no change.
---
- **An open order keeps the flow rules it opened under.** The edge records the package version on
  `sales.order.opened` and checks the order's flow guards against that version, so publishing a
  new version does not change the rules for orders already open. The edge holds the four most
  recently used versions with their conditions compiled once. An order whose version it no longer
  holds, after that limit or a restart, is checked under the current version, and the edge logs it.
