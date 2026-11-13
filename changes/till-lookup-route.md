---
kind: added
upgrade_note: New permission integrations.lookup.run, granted to no role by default; a tenant grants it on the console before anyone at a till can run a lookup. Every box's device credential now carries the call_gateway scope (it gains it at its next request, no reissue), so its edge can make the till's lookups; it still reaches only an endpoint its tenant declared and enabled. New edge route POST /api/gateway/calls and new catalogue act integrations.lookup.run (since 0.14.1).
---
- **The edge runs a till's lookup through the cloud gateway.** `POST /api/gateway/calls` takes an
  endpoint's id and its values, needs the new permission `integrations.lookup.run`, and answers the
  endpoint's declared result or one of `GATEWAY_OFFLINE`, `GATEWAY_TIMEOUT`, `GATEWAY_REFUSED` and
  `GATEWAY_BUSY`. Nothing is kept, no event is written and no value is logged. The catalogue's act
  `integrations.lookup.run` calls it; the till's side follows (ADR-0214).
