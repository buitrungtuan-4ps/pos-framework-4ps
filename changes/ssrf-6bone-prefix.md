---
kind: changed
upgrade_note: A webhook already registered at an address in the 6bone / legacy Teredo block is no longer delivered to; none is expected, since it is reserved address space, not a webhook receiver.
---
- **A webhook may not target the 6bone / legacy Teredo IPv6 block (`3ffe::/16`, RFC 3701, RFC 5156).** `pos-cloud`
  now classifies `3ffe::/16` addresses as `ForbiddenReason::Reserved`, preventing SSRF bypasses via
  6bone testing and legacy Teredo addresses.
