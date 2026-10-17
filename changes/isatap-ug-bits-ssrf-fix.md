---
kind: security
---
- **`webhook::ssrf` refuses ISATAP IPv6 addresses with Universal/Group bit permutations.** ISATAP interface identifiers with `0x0100` or `0x0300` in segment 4 attached to public IPv6 prefixes are now correctly classified.
