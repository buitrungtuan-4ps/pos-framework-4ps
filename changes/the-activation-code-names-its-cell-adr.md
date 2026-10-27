---
kind: added
---
- **ADR-0181, proposed: the activation code names its cell.** A setup sheet's code may carry the
  cell id in front of it (`VN1-XXXX-XXXX-XXXX`), so a replacement box finds its cell without anyone
  typing a cloud address. The prefix is a routing hint, never a credential, and a code without one
  works as today. This is ADR-0179's item C4. It waits on the owner, and no code changes until it is
  accepted.
