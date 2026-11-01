---
kind: changed
upgrade_note: A webhook already registered at an address in the RFC 7600 Dummy IPv6 address block is no longer delivered to; none is expected, since it hosts no webhook receiver.
---
- **A webhook may not target the RFC 7600 Dummy IPv6 Address block (`2001:bb8::/32`).** `pos-cloud`
  now classifies `2001:bb8::/32` addresses as `ForbiddenReason::Reserved`, preventing SSRF bypasses via
  IPv4-recoverable dummy IPv6 addresses.
