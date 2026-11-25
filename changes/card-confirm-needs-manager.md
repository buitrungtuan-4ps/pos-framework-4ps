---
kind: security
upgrade_note: A new setting, `billing.card_confirm_approval`, defaults to `CARD_CONFIRM_APPROVAL_MANAGER`, so after this release confirming a parked card from the terminal's slip needs a manager's badge and PIN in every store. A store that wants the old behaviour sets `CARD_CONFIRM_APPROVAL_NONE`.
---
- **Confirming an unknown card by hand needs a manager** (ADR-0243, security review C1). A `CONFIRM`
  on a parked card is refused `409 APPROVAL_REQUIRED` until a manager types their badge and PIN, as
  for a void, and the manager is recorded beside the capture. The till asks for the manager in place.
  `billing.card_confirm_approval` can set it to `CARD_CONFIRM_APPROVAL_NONE` per store.
