---
kind: security
upgrade_note: A webhook already registered at an address in the 6bone phaseout block is no longer delivered to; none is expected, since 6bone was phased out and reclaimed by IANA.
---
- **A webhook may not target the IPv6 6bone phaseout block (`3ffe::/16`, RFC 3701 / RFC 5156).** `pos-cloud`
  now classifies `3ffe::/16` addresses as `ForbiddenReason::Reserved`, preventing SSRF bypasses via
  reclaimed 6bone testing addresses.
