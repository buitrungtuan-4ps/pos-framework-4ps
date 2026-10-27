---
kind: security
---
- **Refuse RFC 9433 SRv6 SID prefix (`5f00::/16`) in webhook SSRF guard.** SRv6 SIDs are internal
  segment routing instructions rather than public web servers or webhook destinations.
