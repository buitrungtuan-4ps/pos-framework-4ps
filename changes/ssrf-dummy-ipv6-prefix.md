---
kind: changed
upgrade_note: A webhook already registered at an address in the Dummy IPv6 Prefix block is no longer delivered to; none is expected, since it hosts internal OAM / multipoint BFD encapsulation endpoints, not a webhook receiver.
---
- **A webhook may not target the Dummy IPv6 Prefix block (`100:0:0:1::/64`, RFC 9780).** `pos-cloud`
  now classifies `100:0:0:1::/64` addresses as `ForbiddenReason::Reserved`, preventing SSRF bypasses via
  Dummy IPv6 Prefix addresses.
