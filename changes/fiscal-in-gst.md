---
kind: added
---
- **India's GST facts about a bill** (ADR-0227), as pure functions in `pos-country-in::gst`:
  - the place of supply, which is inter-state when the buyer's GSTIN names another state;
  - the GSTR-1 section (B2B, B2CL or B2CS), with the threshold passed in;
  - the shape of an HSN or SAC code, with SAC 9963 for a restaurant;
  - the UPI payment request a till shows as a QR.

  Each is mapped in `countries/in/CONFORMANCE.md` R18 to R21 and is still to be confirmed. Charging
  IGST on the bill, the GSTR-1 export, the item's HSN/SAC field, IRP registration and UPI
  confirmation are not built yet.
