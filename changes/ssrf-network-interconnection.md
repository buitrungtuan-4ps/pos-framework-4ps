---
kind: security
upgrade_note: Webhooks targeting the IPv6 Network Interconnection range (2001:a::/32) are now refused.
---
- **Refuse IPv6 Network Interconnection range (2001:a::/32, RFC 9260) in webhook SSRF validation.** `pos-cloud`
  now refuses `2001:a::/32` addresses to align with the IANA IPv6 Special-Purpose Address Registry and prevent SSRF bypasses via special-purpose network interconnection destinations.
