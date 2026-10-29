---
kind: changed
---
- **The settings, store-group membership and release-list routes answer typed bodies.** The
  settings catalogue, values, effective settings, preset application and setting writes, a store
  group's membership write and the config release list are named in `docs/openapi-admin.json`.
  The bytes on the wire are unchanged.
- **Console schemas no longer share a name with a wire type.** In `docs/openapi-admin.json` the
  cloud's own records are `StoredSettingValue`, `FloorRoutingRule`, `CatalogDisplayCategory` and
  `CatalogDisplaySubcategory`, so they cannot overwrite `pos-proto`'s `SettingValue`,
  `RoutingRule`, `DisplayCategory` and `DisplaySubcategory`. A test fails on any two schemas that
  share a name.
