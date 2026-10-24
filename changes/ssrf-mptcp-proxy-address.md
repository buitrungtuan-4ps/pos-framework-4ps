---
kind: security
---
- **A webhook may not target the IPv6 MPTCP Proxy / TCP Convert Protocol Anycast block.** `pos-cloud`'s
  SSRF filter now classifies `2001:b::/32` (RFC 8803) as `ForbiddenReason::Reserved`, preventing
  webhook destinations from targeting TCP convert service / proxy infrastructure.
