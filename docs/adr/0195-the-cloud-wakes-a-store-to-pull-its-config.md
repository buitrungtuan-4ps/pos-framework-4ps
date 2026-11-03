# ADR-0195 — The cloud wakes a store to pull its config

**Status** Accepted · **Owner** @maintainers-architecture · **Date** 2026-11-03 · Amends
[ADR-0031](0031-cloud-adapter-transports.md) (the link now also carries a wake toward the store, on
the connection the store opens) and [ADR-0039](0039-config-delivery.md) (the push that "merely
triggers the same pull" it foresaw) · Supersedes roadmap-v3 D5 · Relates to
[ADR-0062](0062-the-relay-wake.md), [ADR-0087](0087-edge-relay-and-event-publish.md),
[ADR-0089](0089-edge-event-bus-transport.md),
[ADR-0189](0189-a-store-runs-a-package-version-hq-pins.md)

Accepted by the owner on 2026-11-03 through approval of the overhaul plan
([`docs/roadmap-v4.md`](../roadmap-v4.md)).

## Context

The owner wants a published change on a store in about one second, over a connection the store
opens, with the 30 s poll kept as the fallback.

What exists:

- **The edge pulls every 30 s** (`CONFIG_POLL_INTERVAL`, `crates/pos-edge/src/server.rs`;
  `ConfigClient::run`) from `GET /sync/stores/{store_id}/config`, authenticated by a `read_config`
  key and answered only for the key's tenant ([ADR-0039](0039-config-delivery.md)). A change takes
  15 s on average to arrive.
- **The edge already holds a NATS connection** when `[nats]` is configured: it publishes its outbox
  to the one fleet subject `pos.fleet.events` ([ADR-0087](0087-edge-relay-and-event-publish.md)
  Amendment 1). The store dials out; nothing dials in ([ADR-0031](0031-cloud-adapter-transports.md)).
- **The broker has one fleet-wide token and no per-subject permissions** (`deploy/bootstrap.sh`).
  Per-store mTLS is a later slice ([ADR-0089](0089-edge-event-bus-transport.md)). Any store can read
  or publish any subject.
- **ADR-0062 refused a live link for orders**: a second delivery path that can disagree with the
  first, and data readable and forgeable by every store on that bus. ADR-0039 left room for exactly
  one thing: a push that only triggers the pull.

## Options considered

1. **Send the delta on the bus.** It saves one round trip, about tens of milliseconds. It puts
   prices and menus (T2) on a bus every store can read, makes a second delivery path with its own
   ordering and catch-up, and bypasses the pull's tenant check.
2. **A long-poll on `/sync`.** It needs no bus, but it holds one HTTP connection per store open on
   the cloud's proxy and pool, the cost [ADR-0062](0062-the-relay-wake.md) measured for the relay.
3. **A wake on the bus that triggers the existing pull** (chosen). The pull stays the only way
   configuration arrives.

## Decision

1. **The cloud publishes a wake; the store pulls.**
   - **Subject:** `pos.store.<store_id>.config`, on core NATS. It is not a JetStream subject, creates
     no stream, and is not affected by ADR-0087 Amendment 1's create-or-get stream.
   - **Payload:** a `ConfigWake` wire type in `pos-proto` with one field, `config_version_id`. No
     tenant, no content, no personal data.
   - **When:** after the publish has committed, never before, once per store the publish changed;
     a store-group publish sends one wake per member. A failed publish of a wake is logged and
     counted. It never fails the configuration publish, because the poll still delivers.
2. **The store pulls on a wake, on connecting, and on the poll.**
   - **On a wake:** if `config_version_id` equals the version it holds, it does nothing; otherwise
     it runs the same pull as today, at once.
   - **On every connect and reconnect** to the broker it pulls once. That is the catch-up after being
     offline, and why a wake needs no persistence.
   - **The 30 s poll stays,** unchanged. It covers a lost wake, a store without a bus, and a broker
     outage.
   - **Bounded:** at most one pull runs at a time; wakes that arrive during it collapse into one
     more pull; wake-driven pulls are at most one per second. A flood of wakes costs at most one
     cheap `up_to_date` answer per second.
3. **The wake is a new, receive-only port.** [ADR-0062](0062-the-relay-wake.md) decision 2 keeps
   `MessageLink` outbound only, and it stays so. A new port `StoreWake` in `pos-ports` has one method
   that waits for the next wake or a timeout. `link-nats` implements it on the connection the edge
   already opens; a fake and a contract suite come with it. The cloud's publish side is a function in
   `link-nats`, beside its consumer, called from the config publish path.
4. **Why a wake and not the delta:**
   - **One delivery path.** The pull's tenant check, delta or snapshot choice, and last-known-good
     apply stay the only way configuration reaches a store.
   - **Nothing worth stealing on the bus.** With one fleet token, reading another store's wakes
     shows only that it published; forging one costs that store one pull that answers
     `up_to_date`.
   - **Small.** The extra round trip fits inside the one-second target.
5. **Authentication.** The wake uses the token the edge already holds; no new credential. When
   per-store mTLS lands (ADR-0089), the broker's permissions allow a store to subscribe only to its
   own subject and to publish to none of the `pos.store.>` subjects. The payload stays
   information-free either way.
6. **The target, and what measures it.** A change is applied on the edge within one second of the
   publish, p95, plus the network round trip; the tills then refresh through `config_applied`
   ([ADR-0173](0173-the-till-declares-each-piece-of-store-data-once.md)). The edge records the time
   from receiving a wake to applying the version as a metric, with no personal data.

## Consequences accepted

- **Additions:** one `pos-proto` wire type (additive; `PROTOCOL_VERSION` checked as `pos-proto`
  requires), one port with its fake and contract suite, and a subscription in `link-nats`. No new
  dependency: `async-nats` already offers core subscriptions.
- **A store without a bus keeps the 30 s poll.** The one-second promise holds only where `[nats]` is
  configured, which the pilot requires; the console shows which stores are on the poll alone.
- **The cloud publishes as well as consumes.** Its NATS connection now carries a small outbound
  message per changed store per publish.
- **The relay is untouched.** Orders keep ADR-0061's queue and ADR-0062's in-process wake; this
  record does not reopen the live link ADR-0062 declined.
- **A wake can be lost.** That is accepted: the next reconnect or poll delivers the same version.
