---
kind: security
---
- **The till may connect only to its own edge.** Every page the edge serves carries a content
  security policy whose `connect-src` names the edge alone (and its websocket), so a page served by
  a store's box cannot send data to any other address; an external lookup goes through the edge and
  the cloud (ADR-0198). Nothing the till does today is affected.
