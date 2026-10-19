---
kind: fixed
---
- `classify_v6` in `crates/pos-cloud/src/webhook/ssrf.rs` now classifies `5f00::/8` Segment Routing IPv6 (SRv6) SID addresses (RFC 8986) as `ForbiddenReason::Reserved`, preventing SSRF bypasses via SRv6 Segment Routing addresses. **Upgrade note:** none.
