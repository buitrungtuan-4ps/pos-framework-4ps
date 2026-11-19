---
kind: added
---
- **A guest at a table reads the store's QR menu** (ADR-0224 decision 1). `GET /v1/qr/menu`, with
  the signed table token printed in the QR code, answers the store's published QR-channel menu:
  names in the guest's language, integer prices, availability and modifier groups, and nothing the
  store keeps to itself (tax classes, item codes). The intake's guardrails apply: QR ordering's
  switch, the store online, its business hours and a per-table rate limit, counted apart from the
  table's orders. It takes and answers no personal data.
