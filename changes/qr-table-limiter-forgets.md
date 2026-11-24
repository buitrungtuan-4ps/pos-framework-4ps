---
kind: security
---
- **The guest QR counters forget an idle table.** The per-table counters behind `POST /v1/qr/orders`
  and `GET /v1/qr/menu` drop a table once its window is empty, and above 4,096 tables they forget
  every table idle for a day, so the cloud's memory for them has an explicit bound (phase 9 security
  review, finding G5).
