---
kind: fixed
---
- **The till asks to send unsent food before it takes payment.** *Take payment* on an order, and
  charging a counter order, used to open the bill whatever the lines' state, and settling sends
  nothing to the kitchen, so a guest could pay for dishes the kitchen never got. With lines still
  unsent, the till now asks *Send N items to the kitchen first?* in place: **Send and pay** sends
  them in one transaction and carries on to payment, **Cancel** leaves the order as it was.
