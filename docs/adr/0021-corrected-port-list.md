# ADR-0021 — The sixteen ports

**Status** Accepted · **Owner** @maintainers-architecture · **Last reviewed** 2026-08-18
**Supersedes** [ADR-0006](0006-ports-and-adapters.md)

**Context.** [ADR-0006](0006-ports-and-adapters.md) established the rule that matters —
`pos-core` and `pos-ports` depend only on `std`, `serde` and pure computation crates, every
external system sits behind a port we own, and every port ships a contract-test suite. That
rule is not in question.

Its *list* was incomplete. It named fifteen ports and omitted `OrderIn`, even though
[ADR-0012](0012-qr-ordering-via-cloud.md) is built on it: QR ordering is "architecturally
almost free" precisely because guest orders arrive through the same port as delivery-app
orders and `POST /v1/orders`. `architecture.md` §5 carried the same omission. A port that
three features depend on and no record names is exactly the kind of gap that gets filled by
accident, differently, in two places.

**Options considered.**

1. Edit ADR-0006 in place. Rejected: records are immutable once merged, and that rule is
   worth more than the convenience.
2. Leave `OrderIn` documented only in ADR-0012 and the specification. Rejected: the port
   list is a contract and it needs one authoritative statement.
3. **Supersede with a corrected list.** Chosen.

**Decision.** There are **sixteen** ports. `architecture.md` §5 is the authoritative table;
this record fixes the count and the membership.

| Port | Boundary it owns |
|---|---|
| `EventStore` | Append and read events; the outbox |
| `ConfigStore` | Configuration snapshots and deltas |
| `MessageLink` | Durable store↔cloud channel |
| `BlobStore` | Large objects: backups, OTA artifacts |
| `MetricsSink` | Numeric telemetry |
| `Signer` | Signature verification |
| `KeyVault` | Key storage |
| `ClockSource` | Time, and drift detection |
| `IdGenerator` | ULID generation |
| `PrinterDriver` | ESC/POS and the print queue |
| `PaymentTerminal` | Card terminals |
| `Fiscalization` | Legal invoicing, per country |
| `DeliveryVendor` | Delivery marketplaces |
| `ShippingDispatch` | Couriers |
| `ErpSink` | ERP posting |
| **`OrderIn`** | **Orders originating outside the store: marketplaces, the public API, QR ordering** |

`ClockSource` and `IdGenerator` are synchronous and are the only two `pos-core` holds; the
rest are asynchronous and belong to the binaries ([ADR-0013](0013-async-strategy.md)).

**Consequences.**

- The count is settled, so the port inventory, the contract-test matrix, and the adapter
  template all have one number to agree with.
- `BlobStore` is deliberately kept thin: [ADR-0007](0007-in-house-vs-dependency.md) plans to
  delete it outright once WAL shipping is in-house, because object storage exists only to
  satisfy Litestream. Do not invest in its abstraction.
- `Signer` and `KeyVault` are listed separately here where ADR-0006 hyphenated them. They
  are separate role-shaped ports: verifying a signature and storing a key are different
  jobs, and interface segregation says an adapter should not be made to implement both.
- Adding a seventeenth port still requires an ADR first. That rule is unchanged.
