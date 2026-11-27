---
kind: added
upgrade_note: "Two new settings on a new `order_channels` node (ADR-0260): `order_types` (default: the three built-in types, as before) and `sources` (default: none beyond the built-ins). A store that sets neither runs as before. The cloud refuses a write that drops a token the same scope held; set it inactive instead."
---
- **HQ can list its own order types and sale sources.** `order_channels.order_types` holds up to 32
  types, each mapped onto dine-in, takeaway or delivery, named in English and Vietnamese, active or
  not, one the default; `order_channels.sources` holds up to 64 sources such as a hotline or a
  website. Both are set from the tenant down to one store (ADR-0260).
