---
kind: added
---
- **The console prints a QR code for each table** (ADR-0253). Floor → **Table QR sheet** now draws a
  card per active table with its name, its QR code and the guest address the code holds, and
  **Print the sheet** prints them, three to a row, for standing on the tables. Before, the sheet
  listed each table's token as text only.

  **Upgrade note (dependency):** `dashboard/` gains `qrcode-generator` 2.0.4, the till's encoder,
  loaded on demand by this sheet only.
