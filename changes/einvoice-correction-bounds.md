---
kind: security
---
- **A correction must change something** (security review E1). `POST /admin/einvoices/{invoice_id}/correct`
  refuses an adjustment of zero and a replacement with a negative total, `400 OUT_OF_RANGE` on
  `total_minor`.
