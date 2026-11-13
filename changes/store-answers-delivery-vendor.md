---
kind: added
upgrade_note: New store-facing route POST /sync/stores/{store_id}/delivery/answers (scope relay_orders). The demo delivery provider gains an optional `answer_url` setting; a connection without one cannot be answered (404). The OrderConnector seam gains `answer`.
---
- **A store answers its delivery vendor through the cloud.** Accept with a preparation time,
  reject with a reason code, ready, or busy go to the vendor's connector, which signs them for the
  connection's answer URL; the cloud sends them behind the webhook guard with an idempotency key
  (ADR-0215).
