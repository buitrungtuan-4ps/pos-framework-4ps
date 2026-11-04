---
kind: security
---
- **Webhook SSRF protection classifies the RFC 2928 `2001::/23` IETF Protocol Assignments range.**
  Addresses in `2001::/23` that have no explicit public unicast allocation are now refused
  as `ForbiddenReason::Reserved`.
