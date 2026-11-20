---
kind: security
---
- **A webhook may not target the 6bone phaseout IPv6 block (`3ffe::/16`, RFC 3701 / RFC 5156).**
  `pos-cloud` webhook SSRF validation now classifies `3ffe::/16` addresses as `ForbiddenReason::Reserved`,
  preventing SSRF payloads targeting returned 6bone / legacy Teredo address space from bypassing validation.
