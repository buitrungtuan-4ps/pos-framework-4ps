---
kind: fixed
upgrade_note: A webhook already registered at an address in the fec0::/10 site-local range will no longer be delivered to; none is expected as site-local addresses are deprecated and non-routable.
---
- **Webhook SSRF protection now refuses IPv6 site-local addresses (`fec0::/10`).**
  `classify_v6` in `crates/pos-cloud/src/webhook/ssrf.rs` now classifies deprecated IPv6 site-local addresses
  (`fec0::/10`, RFC 3879) as `ForbiddenReason::Private`, preventing SSRF bypasses via site-local IPv6 addresses targeting private internal networks.
