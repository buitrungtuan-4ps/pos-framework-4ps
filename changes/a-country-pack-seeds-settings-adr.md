---
kind: added
---
- **ADR-0183, accepted: a country pack seeds a new store's settings in its own currency.** A setting
  counted in money, such as the drawer over/short that asks for a reason, gets its new-store value
  from the store's country pack instead of a single number for every currency. Vietnam's first value
  is 20,000 ₫. Every value stays editable per tenant, brand, store group or store. The code follows
  in its own change.
