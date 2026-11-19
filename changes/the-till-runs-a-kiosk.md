---
kind: added
---
- **A till on a kiosk terminal runs self-order** (ADR-0224 decisions 4 to 6). It signs in as the
  kiosk's own record when it starts and draws only the menu, the guest's order and the number the
  order is called by (`kiosk_menu`, `kiosk_cart`, `kiosk_done` in the screen catalogue). A guest's
  first tap opens a tableless order; they add and take back dishes and send the order to be paid at
  the counter, which opens its bill. An order nobody touches for `kiosk.abandon_seconds` is given
  up: its lines voided and the order closed citing `ABANDONED` where the store's reason list gives
  it, and otherwise left open for staff. With no shift open the kiosk says it is not taking orders.
  A member of staff leaves the kiosk with their code and PIN, which takes `admin.kiosk.exit`.
