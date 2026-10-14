---
kind: changed
upgrade_note: A webhook already registered at an address in one of these blocks is no longer delivered to; none is expected, since none of them hosts a webhook receiver.
---
- **A webhook may not target the rest of IANA's special-purpose address blocks.** `pos-cloud`
  already refused the IPv6 AMT and AS112 blocks; for consistency with the IANA special-purpose
  registries it now also refuses `192.52.193.0/24` (AMT, RFC 7450), `192.31.196.0/24` (AS112-v4,
  RFC 7535), `192.175.48.0/24` and `2620:4f:8000::/48` (Direct Delegation AS112, RFC 7534),
  `2001:5::/32` (LISP EID space, RFC 7954) and `2001:30::/28` (DRIP Entity Tags, RFC 9374).
  Registering or delivering to one is refused as `RESERVED`.
