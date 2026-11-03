# ADR-0198 — A store reaches the internet only through the cloud

**Status** Accepted · **Owner** @maintainers-architecture · **Date** 2026-11-03 · Extends
[ADR-0153](0153-a-vendor-is-a-provider-the-cloud-chooses.md) (decision 1, from vendor families to
every external call) · Relates to [ADR-0032](0032-webhooks.md),
[ADR-0038](0038-webhook-tls-sender.md), [ADR-0083](0083-integration-doctrine.md),
[ADR-0191](0191-every-act-is-in-a-typed-catalogue.md),
[ADR-0194](0194-custom-data-is-declared-and-personal-fields-are-kept-apart.md)

Accepted by the owner on 2026-11-03 through approval of the overhaul plan
([`docs/roadmap-v4.md`](../roadmap-v4.md)).

## Context

A package will want to look something up or tell another system: check a loyalty balance, validate
a voucher, send a reservation to a booking service. The owner decided that a till never calls the
internet; external calls go only through the cloud, as a gateway.

What exists:

- **ADR-0153** (Proposed) puts an internet service's adapter in the cloud, as a connector chosen by a
  tenant's connection, with secrets encrypted, write-only and never on a store.
- **The cloud already dials out safely** for webhooks and alerts: `crates/pos-cloud/src/webhook/ssrf.rs`
  vets every destination before registering it and before every delivery, refusing private,
  loopback, link-local and other non-public ranges in IPv4 and IPv6, and the sender follows no
  redirect ([ADR-0032](0032-webhooks.md), [ADR-0038](0038-webhook-tls-sender.md)).
- **Every act is in a catalogue** with an `ACTION_EFFECT_EXTERNAL` class
  ([ADR-0191](0191-every-act-is-in-a-typed-catalogue.md)).

## Options considered

1. **The edge calls out directly.** Secrets on every box, and every store's network becomes a path
   to whatever a package names.
2. **A connector per use, written in Rust.** Safe, but every lookup is a cloud release.
3. **One gateway in the cloud: connectors for vendor families, and tenant-declared HTTPS endpoints
   for simple lookups, both behind the same guard** (chosen).

## Decision

1. **The path.** An external act goes from the till to its edge, from the edge to the cloud over the
   store-facing surface (`POST /sync/stores/{store_id}/gateway/calls`, a new `call_gateway` key
   scope), and from the cloud to the target. The edge serves the till with a
   content security policy whose `connect-src` names only the edge, which it does not set today.
2. **Two kinds of target.**
   - A **connector** from ADR-0153's catalogue, through a tenant's connection.
   - A **gateway endpoint** the tenant declares: an `https` URL, a method, a request built from typed
     values by field name, and a response read into a typed result schema. It is data, never code.
3. **Secrets stay in the cloud**, encrypted and write-only as ADR-0153 decision 4 says. A package or
   a store names an endpoint by id and never holds its credential.
4. **The guard is the webhook guard.** Every call goes through `ssrf.rs`'s vetting: `https` only, the
   resolved address checked and pinned, private and other non-public ranges refused, no redirect
   followed. A response is at most 256 KiB.
5. **Timeouts.** Each endpoint states a deadline, 3 s by default and at most 10 s. The edge waits no
   longer than the deadline plus one second. The till shows the act pending meanwhile
   ([ADR-0196](0196-every-act-paints-first-and-money-waits-for-the-edge.md)).
6. **Retries and idempotency.**
   - The edge mints an `idempotency_key` per act, and the cloud passes it to the target as
     `Idempotency-Key`.
   - The cloud retries a transport failure or a `5xx` at most twice, inside the deadline, only for an
     endpoint declared idempotent. Others are tried once.
   - A repeated key within 24 hours returns the first answer, so a till retried by a person does not
     act twice.
7. **Bounded.** At most 10 calls a second per store and 50 per endpoint per tenant; past that the
   call is refused at once. An endpoint that keeps failing opens a circuit breaker, as webhooks do.
8. **Offline is declared.** When the cloud is unreachable, an external act fails at once with a
   sentence. A required flow step that is external must declare its offline branch (skip with a
   note, or another step), or the version is refused at publish.
9. **Personal data.** A request may include a personal field ([ADR-0194](0194-custom-data-is-declared-and-personal-fields-are-kept-apart.md))
   only if the endpoint is declared as a processor with a data processing agreement reference and
   the country it sends data to; the console warns on a transfer outside the store's country. Logs
   record the call id, endpoint id, status and duration, never a body.

## Consequences accepted

- **New:** one store-facing route, one key scope, the till's `connect-src` policy, an endpoint
  registry in the cloud with its admin routes, and an idempotency record kept 24 hours. No new
  dependency.
- **An external act depends on the cloud.** That is the intent; a store keeps selling without it.
- **Lookups that need logic** beyond mapping fields need a connector or a function (ADR-0199).
- **ADR-0153 stays Proposed** for what it alone decides (the provider catalogue and the
  `integrations` node). This record decides only that every external call goes through the cloud.
