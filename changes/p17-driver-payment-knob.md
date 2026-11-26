---
kind: changed
---
- **The P17 load driver takes the tender to pay with** (`PAYMENT`, default `PAYMENT_METHOD_CASH`). A
  store that keeps a drawer per till refuses cash at a device bound to no till, as the driver's are,
  so on such a store (the phase 10 Vietnamese store) it runs with `PAYMENT=PAYMENT_METHOD_QR`.
