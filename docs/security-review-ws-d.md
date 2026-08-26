# Security review — WS-D ops hardening

**Status** Accepted · **Owner** @maintainers-security · **Last reviewed** 2026-08-26

A focused security pass over the surface this PR adds or changes — guest QR ordering (#89), the
edge relay and config-pull clients (#100, #101), the durable KDS bump (#44), and the ops-hardening
config validation and metrics wiring (#103). Full-fleet review remains the P13/production posture in
`SECURITY.md`; this note records what was checked here and the one gap it closed.

## Finding closed — a bad menu node validated but was silently dropped

`PUT /admin/stores/{id}/config/{level}` accepts an arbitrary JSON document and publishes it through
`CapabilityValidator`, which checked only the capability flags and the OTA keys — never the compiled
`menu`/`layout` delivery nodes. A document whose `menu` node the edge could not parse therefore
**validated and published**, and the edge's forgiving `session_from_config` then left the price book
unchanged with no error surfaced anywhere: a "successful" publish that never reached the counter.

- **Severity:** low. The route is behind `authenticate_session` (super-admin only), so this is an
  integrity/robustness gap — a privileged operator silently shipping a no-op — not an unauthenticated
  vulnerability. It matters because a silent config no-op is exactly the kind of drift an ops team
  cannot see.
- **Fix (this PR):** `CapabilityValidator` now round-trips any `menu`/`layout` node through the edge's
  exact `to_string` → `from_str` path and rejects a publish the store could not consume, on **every**
  publish path (the generic route and the catalog compile route alike). Tested both ways
  (`config_tree::validate`).

## Surfaces reviewed and upheld

- **Guest QR token (#89) — the sole credential for an unauthenticated order.** Verification uses
  `Mac::verify_slice` (constant-time), and the token binds `store_id` + `table_id`, so a signature
  cannot be forged without the server secret and a token minted for one table cannot be replayed on
  another. The table token is a persistent printed credential by design; abuse is bounded by the
  guardrails — per-table rate limit, business-hours gate, online-only gate, and staff-confirmation on
  by default — not by token expiry. The endpoint is off unless `table_token_secret` is configured.
- **Edge relay + config clients (#100, #101) — inbound untrusted data.** A malformed relayed order is
  acked as `invalid_argument` rather than dropped or panicked; a malformed or absent config node
  leaves the running session unchanged rather than blanking a trading store's menu. Both fail closed.
- **Metrics (#103) — no PII, no cardinality.** The `MetricLabelValue` alphabet forbids names, emails,
  phones and identifiers by construction (a compile-time assertion stops a label ever holding a ULID),
  and the heartbeat this PR wires carries **no labels at all**. The sink is off by default, off the
  sales path, and its bounded queue drops under pressure — a metrics backend cannot become a trading
  outage. The import URL is operator-set plain `http` to the box's own private network (TLS
  terminates at the proxy, P8), not user-controlled, so there is no SSRF surface.
- **Logging.** The request-tracing layer records method, path and status, never a request body, and
  configuration and metrics carry no personal data — consistent with the PDPD/telemetry posture in
  `docs/roadmap.md` A6 (machine data only; no employee-behaviour monitoring).

## Not in scope here

Cross-tenant RLS, ingest idempotency, webhook SSRF/HMAC, and the super-admin TOTP/session model were
reviewed in their own phases (P7, ADR-0032/0034) and are unchanged by this PR. The production-wide
review and the human/hardware gate register are WS-F / P13.
