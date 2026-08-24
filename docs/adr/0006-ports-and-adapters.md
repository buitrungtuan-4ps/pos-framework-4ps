# ADR-0006 — Own the boundary, not the implementation

**Status** Superseded by [ADR-0021](0021-corrected-port-list.md) · **Owner** @maintainers-architecture · **Last reviewed** 2026-08-18

> The decision in this record stands. Only its port *list* was incomplete — it omitted
> `OrderIn`. ADR-0021 restates the list in full.

**Context.** The goal is a framework that is not hostage to any vendor or library, without rewriting databases and runtimes.

**Decision.** `pos-core` and `pos-ports` depend only on `std`, `serde` and pure computation crates; a CI test enforces the allow-list. Every external system sits behind a port: `EventStore`, `ConfigStore`, `MessageLink`, `BlobStore`, `MetricsSink`, `Signer`, `KeyVault`, `ClockSource`, `IdGenerator`, `PrinterDriver`, `PaymentTerminal`, `Fiscalization`, `DeliveryVendor`, `ShippingDispatch`, `ErpSink`. Each port ships a shared contract-test suite that every implementation must pass.

**Consequences.**
- Domain tests run in milliseconds with in-memory fakes — no database, no network, no hardware.
- Replacing NATS, S3 or any vendor is an adapter change plus one wiring line in a binary.
- Slightly more indirection, and the discipline must hold from the first commit; adding it later is expensive.
