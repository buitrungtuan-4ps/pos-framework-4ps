---
kind: security
---
- **Refuse ISATAP interface identifiers with non-zero universal/group bits in webhook SSRF validation.** `pos-cloud` now checks `(segments[4] & 0xfcff) == 0 && segments[5] == 0x5efe` when validating IPv6 addresses, preventing SSRF bypass via ISATAP interface identifier variants (`0100:5efe` or `0300:5efe`).
