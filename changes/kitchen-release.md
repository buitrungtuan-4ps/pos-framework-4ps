---
kind: added
upgrade_note: The till's button on a waiting guest or delivery order now reads "Accept order" instead of "Send to kitchen", because it only accepts the order. Nothing else changes until a store sets `qr.kitchen_release` or a delivery connection's `kitchen_release`.
---
- **A store chooses how a QR or delivery order reaches the kitchen** (ADR-0247, phase 10 open item
  O4). `qr.kitchen_release`, and `kitchen_release` on each delivery connection, take
  `KITCHEN_RELEASE_CONFIRM_THEN_SEND` (the default: accept, then send), `_CONFIRM_FIRES` (one tap
  accepts and sends, and prints the tickets) or `_AUTO_ACCEPT_AND_FIRE` (no tap: the order is sent
  as it arrives). The till names its button by what it does.
