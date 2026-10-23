---
kind: changed
upgrade_note: A webhook already registered at an address in the SRv6 locator block is no longer delivered to; none is expected, since it hosts Segment Routing infrastructure, not a webhook receiver.
---
- **A webhook may not target the IPv6 SRv6 Locator block (`5f00::/16`, RFC 8986).** `pos-cloud`
  now classifies `5f00::/16` addresses as `ForbiddenReason::Reserved`, preventing SSRF bypasses via
  Segment Routing locator addresses.
