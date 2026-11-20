---
kind: security
---
- **A webhook may not target IPv6 Subnet-Router or Reserved Subnet anycast addresses.** `pos-cloud` webhook SSRF validation now classifies Subnet-Router anycast addresses (RFC 4291 §2.6.1, interface identifier `::0`) and Reserved Subnet Anycast addresses (RFC 2526, interface identifier `fdff:ffff:ffff:ff80..=ffff`) as `ForbiddenReason::Reserved`, preventing SSRF payloads from targeting subnet routers or anycast infrastructure.
