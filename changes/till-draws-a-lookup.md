---
kind: added
upgrade_note: Protocol, additive. The component catalogue gains `lookup_field` and `lookup_result`, a property kind PROPERTY_KIND_FIELD, and `action_button` now supplies `values`, so a package's button may bind integrations.lookup.run with only the endpoint fixed. A version naming them is floored at 0.14.1. The till needs integrations.lookup.run, granted to no role by default.
---
- **A till draws a lookup through the gateway.** A package screen's `lookup_field`s take what a
  person types, its button runs `integrations.lookup.run`, and its `lookup_result`s draw each field
  of the answer under its label. The answer is held for the screen only and never kept, logged or
  put in an event; `GATEWAY_OFFLINE`, `GATEWAY_TIMEOUT`, `GATEWAY_REFUSED`, `GATEWAY_BUSY` and an
  unknown endpoint are said in the till's language (ADR-0214).
