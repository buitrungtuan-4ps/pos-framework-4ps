---
kind: added
---
- **A package version has a wire shape, and a till's variant is chosen by one rule.** `pos-proto`
  gains the `package` node (the pin: `PACKAGE_MODE_LOCKED` or `PACKAGE_MODE_CHOOSE` with its
  version ids, and the pinned version's `content`), `PackageId`, `PackageVersionId`, the
  `DeviceClass` wire enum, and the variant selector over device, role, sales channel and time slot.
  `pos-core` resolves a variant and checks a part's variants (ADR-0189 decision 4), against vectors
  in `crates/pos-proto/testdata/package_variants.json` that the till will run too. Nothing publishes
  or reads the node yet, so no store behaves differently.
