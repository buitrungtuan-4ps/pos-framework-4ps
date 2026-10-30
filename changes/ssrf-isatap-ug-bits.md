---
kind: security
---
- **A webhook may not target ISATAP IPv6 addresses with non-zero u/g modifier bits.** `pos-cloud` webhook SSRF validation now inspects ISATAP interface identifiers across all modifier bit combinations (`segments[4] & 0x00ff == 0`), preventing SSRF payloads embedding private or loopback IPv4 addresses from bypassing validation.
