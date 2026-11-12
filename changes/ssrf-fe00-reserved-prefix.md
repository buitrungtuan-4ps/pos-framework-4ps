---
kind: security
---
- **A webhook may not target the IPv6 IETF reserved block (`fe00::/9`, RFC 4291).** `pos-cloud` webhook SSRF validation now inspects and classifies `fe00::/9` addresses as `ForbiddenReason::Reserved`, preventing SSRF bypasses targeting reserved IPv6 address space.
