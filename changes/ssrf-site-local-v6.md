---
kind: changed
upgrade_note: A webhook destination pointing at an IPv6 site-local address (fec0::/10) is now refused as a private network address.
---
- **A webhook may not target IPv6 site-local addresses (`fec0::/10`).** `pos-cloud` webhook SSRF
  validation now classifies IPv6 site-local addresses (RFC 3513 / RFC 3879) as private network
  addresses (`ForbiddenReason::Private`), preventing SSRF payloads from probing site-local IPv6 intranet services.
