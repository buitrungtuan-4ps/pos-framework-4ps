# `link-nats`

**Layer** `adapter` · **Owners** @maintainers-cloud

## What it is

Outbound, at-least-once event delivery over NATS JetStream: the store publishes each event to its stream, and the cloud consumes idempotently by ULID through `NatsConsumer`. The edge constructs the link only when a `[nats]` section is configured.

## Owns

- `MessageLink` over JetStream (`NatsLink`)
- The cloud's durable consumer (`NatsConsumer`)

## Depends on / used by

- **Depends on** (workspace crates): `pos-ports`, `pos-proto`; in tests, `pos-contract-tests`.
- **Used by** (workspace crates): `pos-cloud`, `pos-edge`.

## Entry points

- `src/link.rs`: `NatsLink`, `NatsConfig`
- `src/consumer.rs`: `NatsConsumer`

## Rules

- [ADR-0024](../../../docs/adr/0024-protocol-version-negotiation.md) · [ADR-0026](../../../docs/adr/0026-port-shapes.md) · [ADR-0031](../../../docs/adr/0031-cloud-adapter-transports.md) · [ADR-0087](../../../docs/adr/0087-edge-relay-and-event-publish.md)

## How to test

```bash
cargo test -p link-nats
NATS_URL=127.0.0.1:4222 cargo test -p link-nats --features integration
```
