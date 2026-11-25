---
kind: added
---
- **The receipt and the till show the e-invoice's number and lookup code** (ADR-0242). A settle that
  issues its e-invoice answers with `invoice_number` and `invoice_lookup_code`; the till's paid panel
  shows both and the printed receipt prints them under the totals. The buyer form gains an email,
  kept apart as personal data, and warns when a tax number does not look like one.
