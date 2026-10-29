---
kind: added
---
- **An activation code may name its cell.** `pos-core`'s `ActivationCode` reads an optional cell-id
  prefix, `VN1-XXXX-XXXX-XXXX` (ADR-0181, ADR-0179 C4): the last twelve symbols are the code, read as
  before, and what comes before them, less one hyphen, is a cell id, read case-insensitively and
  never folded, so `jp-east-2` and `us1` survive. The code is redeemed by its twelve symbols alone,
  so a prefix never changes which code is spent. A prefix that is not a cell id, or one with no
  hyphen before the code, is refused with its own reason. Every code without a prefix parses exactly
  as before. Issuing a prefixed code, and acting on one at the store, come in later changes.
