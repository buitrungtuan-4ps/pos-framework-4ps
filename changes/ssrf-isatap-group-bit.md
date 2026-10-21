---
kind: security
---

- Refuse ISATAP interface identifiers with group-bit / multicast-modified flags (`0x0100`, `0x0300`) containing loopback/private IPv4 targets in webhook SSRF validation.
