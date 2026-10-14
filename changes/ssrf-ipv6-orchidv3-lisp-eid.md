---
kind: security
upgrade_note: none
---
- **SSRF check blocks ORCHIDv3 and LISP EID IPv6 ranges.** `classify_v6` in `crates/pos-cloud/src/webhook/ssrf.rs` now classifies `2001:30::/28` (ORCHIDv3, RFC 9389) and `2001:5::/32` (LISP EID, RFC 7982) as `ForbiddenReason::Reserved`.
