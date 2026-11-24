---
kind: security
upgrade_note: A guest QR order's `external_reference` is now the token's table id, a `:` and the reference the guest page sent, which may be at most 101 bytes. Nothing but the guest page sends one, and it sends a 36-character UUID.
---
- **A guest QR order's reference names its table.** The intake is idempotent on the reference
  within a store and channel, so `POST /v1/qr/orders` puts the token's table in front of the
  guest's reference: a reference sent from another table is another order, never that table's
  order and total (phase 9 security review, finding G4).
