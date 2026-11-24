---
kind: added
upgrade_note: billing.payment.captured gains an optional terminal_reference, and the edge now counts a payment only when its outcome is PAYMENT_OUTCOME_CAPTURED. An edge rolled back below this release would count a parked (unknown) card as paid, so resolve every parked card before rolling an edge back.
---
- **A card is taken through the card terminal** (ADR-0230): `POST /api/bills/{id}/card` asks the
  terminal, records a capture as a payment, records nothing for a decline, and parks the bill on an
  unknown result until it is looked up, confirmed or cancelled
  (`POST /api/bills/{id}/card/{payment_id}/resolve`). A kiosk may pay its own bill by card. Only the
  fake terminal exists yet; a real adapter waits on the owner's choice of vendor.
