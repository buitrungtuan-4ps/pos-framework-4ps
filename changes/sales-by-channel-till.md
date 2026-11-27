---
kind: added
upgrade_note: The till's X (`GET /api/shifts/{id}/x`, `GET /api/shifts:x`) adds `channels` and, under `tenders`, `in_app` and `other_than_apps`; `tenders.other` keeps its meaning (in-app payments included). The printed X now prints "Other" without in-app payments.
---
- **The till's X reports sales by channel and order type, and names each delivery app
  (ADR-0259).** Its answer lists the bills by channel, app and type, the same lines the console's
  X/Z shows, and each app's in-app payments; the printed X reads "Trả qua GrabFood" instead of
  counting app money under "Khác".
