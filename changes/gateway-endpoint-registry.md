---
kind: added
upgrade_note: Migration 0098 adds the gateway_endpoints table. A secret header needs integration_secret in cloud.toml, as a connection's secret does.
---
- **A tenant can declare the internet endpoints its stores will look things up through.** The
  console API `/admin/integrations/gateway-endpoints` stores an `https` URL, a method, the typed
  fields a request is built from and a response is read into, a deadline (3 s by default, at most
  10 s), and secret headers that are sealed on arrival and never shown again. A URL that is not
  `https` or points at a private address is refused. Nothing calls an endpoint yet (ADR-0198).
