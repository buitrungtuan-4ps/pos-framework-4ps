---
kind: added
---
- **The cell directory and its signature check exist as a library.** `pos-cloud` can read the one
  JSON directory every cell carries (tenant slug, country with a `"*"` fallback, and cell id to base
  URL, with no personal data), check its minisign signature against the directory key's public half,
  check its content and accept only a higher version, keeping the last good directory and the reason
  for any refusal (ADR-0179 C2). Nothing reads a directory yet: the `[cell]` keys and the boot load
  follow in the next change.
