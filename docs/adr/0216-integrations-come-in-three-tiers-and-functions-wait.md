# ADR-0216 — Integrations come in three tiers, and WebAssembly functions wait for a real case

**Status** Accepted · **Owner** @maintainers-architecture · **Date** 2026-11-13
· Defers [ADR-0199](0199-a-function-is-signed-pure-webassembly-that-returns-a-decision.md) ·
Relates to [ADR-0083](0083-integration-doctrine.md), [ADR-0198](0198-a-store-reaches-the-internet-only-through-the-cloud.md),
[ADR-0215](0215-a-delivery-order-names-its-vendor.md)

## Context

Phase 5 of [roadmap-v4](../roadmap-v4.md) asks for a signed WebAssembly function on a canary ring.
ADR-0199 decided how a function runs, but left its first case to the owner, and no case has come
up that the public API or a cloud connector cannot serve. A function brings a runtime dependency,
a signing key and its custody, a review process for code written outside the team, and tooling for
its authors. The roadmap's own principle is "only when a real case needs it".

## Options considered

1. **Build functions now** with a made-up first case. Every cost above, for no user.
2. **Drop functions.** Closes a door that a franchise or partner ecosystem may need later.
3. **Integrate in three tiers, and defer functions until a real case** (chosen).

## Decision

1. **Tier 1, the public API and signed webhooks**, is how anyone who follows our contract
   integrates: `/v1` with an API key, and webhooks filtered by event type. A delivery aggregator,
   a loyalty app or an accounting tool uses this tier.
2. **Tier 2, a cloud connector per vendor that will not follow our contract** (a marketplace with
   its own fixed format, such as a delivery app), is code in the cloud behind the `OrderConnector`
   seam (ADR-0083 decision 3, ADR-0215 decision 6). Adding one is a cloud deploy, never a store
   release, in any country.
3. **Tier 3, WebAssembly functions (ADR-0199), is deferred.** ADR-0199 stays the design to use;
   nothing of it is built, no `wasmtime` dependency is added and no function-signing key is
   created until the owner names a real case that tiers 1 and 2 cannot serve.
4. **Phase 5's exit criterion** becomes: a till's external lookup and a delivery order both go
   through the gateway, and a partner places an order through the public API.

## Consequences accepted

- Logic that differs per tenant is configuration (packages, CEL) or a connector the team writes,
  not code a tenant uploads.
- A partner whose integration needs custom code waits for a case to be named and for ADR-0199 to
  be built.
- A connector written for a third party is the team's code to review and maintain.
