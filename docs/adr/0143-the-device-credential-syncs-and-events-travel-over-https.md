# ADR-0143 — One credential per box: the device credential syncs, and events travel over HTTPS

**Status** Accepted · **Owner** @maintainers-architecture · **Date** 2026-09-24
· Completes [ADR-0118](0118-one-credential-per-box-and-the-cloud-learns.md) · Relates to
[ADR-0051](0051-device-credential-provisioning.md), [ADR-0087](0087-edge-relay-and-event-publish.md),
[ADR-0141](0141-the-console-hands-out-the-installer-by-name.md)

## The problem

Activation mints one credential per box (ADR-0118), and that credential authenticates nothing. A box
also needs two more secrets before it syncs:

- **A store API key for `/sync`.** It is pasted at the setup window's `-AskSyncKey` prompt
  (ADR-0141) or put in an env file.
- **The fleet's NATS token for its events.** `bootstrap.sh` writes one `authorization { token }` with
  no per-subject permissions, so any box holding the token can publish as **any** store. The cloud has
  to expose port 4222 publicly for every store.

The step-1.2 goal is that a technician needs only the installer and one code, and that one leaked box
does not expose the chain.

## Options considered

1. **Activation hands out a store key and per-store NATS credentials.** This needs NATS operator/JWT
   accounts, a new `nkeys` dependency and a key-issuing service, and it keeps 4222 public. It also
   changes `ActivationGrant`, which is a port change.
2. **The device credential authenticates `/sync`, and events move to an HTTPS route under `/sync`.**
   No new dependency, no port change, and no public broker port. NATS stays for deployments that run
   it.
3. **Leave it.** Two secrets keep travelling by hand, and one box can publish as every store.

## Decision

**Option 2.**

- **`/sync/*` accepts `Bearer posdev_…`.** The cloud looks the credential up by its id, compares
  `SHA-256` of the secret (ADR-0051), and grants exactly `read_config`, `relay_orders` and
  `publish_events`, **for the credential's own store**. `require_store` applies as it does to a store
  key. A store API key keeps working unchanged.
- **Archiving the device revokes it.** A credential whose device is `archived` in the registry
  (Activation → Registered devices → Archive) no longer authenticates. There is no second revocation
  list to keep in step.
- **`POST /sync/stores/{store_id}/events`** (scope `publish_events`) takes
  `{ "events": [EventEnvelope, …] }`: at most 256 envelopes and 4 MiB.
  - Every envelope must name the path's store. A batch with any other store in it is refused whole,
    so no box can write another shop's history.
  - Ingest is the existing idempotent `Cloud::ingest`.
  - The answer is `{ "accepted": n }`, a prefix, as the `MessageLink` contract requires.
  - `POST /sync/stores/{store_id}/events/hello` runs the ADR-0024 negotiation on the cloud's side.
- **The edge picks its link from its configuration.** With `nats_url` set it publishes to NATS as
  before. Without it, it publishes through a new `HttpLink` adapter (in `cloud-sync-http`, over the
  same TLS transport), and it passes the shared `MessageLink` contract suite.
- **The edge finds its key in this order:** the vault's sync key, `POS_EDGE_SYNC_KEY`, then the
  vault's device credential. `pos-edge install` stops passing `-AskSyncKey`. The switch stays on the
  script, so nothing that calls it breaks.
- **The new `publish_events` scope** can also be granted to a store API key, for a fork that prefers
  keys.

## Consequences accepted

- **A box needs the installer and one code.** The store key and the NATS token no longer travel, and
  a store's events can come only from a box holding that store's credential.
- **Event traffic moves onto the cloud's HTTPS front.** It shares the `/sync` budget (600 requests a
  minute per client by default). At 256 events a batch, that drains a long outage at about 2,500
  events a second, well above any store's rate.
- **Over HTTPS, an acknowledgement means the cloud ingested the event.** Over JetStream it means the
  stream accepted it. The first is stronger, and retention (ADR-0145) reads it as "synced" either way.
- **Existing NATS deployments are untouched.** Moving one is a configuration change: remove
  `nats_url`.
- **Revocation is as coarse as the registry:** one device, all of its credentials. That is what the
  console already shows.
