---
kind: added
---
- **A delivery vendor's own webhook can place an order at a store.** A tenant connects a delivery
  vendor to one store with the vendor's signing secret and an item map; the vendor's signed order
  arrives at `POST /connectors/{tenant_id}/{connection_id}/orders`, is checked, mapped to the menu,
  and reaches the store as a delivery order through the same relay as `/v1/orders`. The first
  connector is a sandbox demo vendor; the order carries no vendor id or address yet.
