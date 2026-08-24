# Write an adapter

**Status** Accepted · **Owner** @maintainers-architecture · **Last reviewed** 2026-08-21

An adapter connects one external system — a payment terminal, a courier, a marketplace, an ERP — to a
**port** the core already defines. You never touch `pos-core`; you implement a trait and prove it with
the port's shared contract suite. If the suite passes, the adapter is swappable-in.

## The shape (every adapter is the same three parts)

1. **A transport seam** — the one place that touches the socket, plus a TLS sender on the tree's stack.
2. **A pure core** — build the request, map the provider's replies to the port's types and its HTTP
   status to a `PortError`. No I/O here, so it is fully unit-testable.
3. **A stub-driven contract test** — a fake transport that speaks the provider's wire, driving the
   port's shared suite from `pos-contract-tests` with no network.

## Steps

1. **Copy the template.** It is a scaffold, not a crate — the files carry a `.tmpl` suffix so the build
   ignores them until you fill them in.
   ```bash
   cp -r templates/adapter-template crates/adapters/<your-adapter>
   cd crates/adapters/<your-adapter>
   for f in $(find . -name '*.tmpl'); do mv "$f" "${f%.tmpl}"; done
   ```
2. **Fill in the placeholders** (`{{...}}`): the crate name, the port you implement, the provider's
   wire. The template's own [`README`](../../templates/adapter-template/README.md) has the full
   checklist; the load-bearing part is mapping each provider status to the right `PortError` — that is
   what the port's contract turns on.
3. **Register the crate**: add `"crates/adapters/<your-adapter>"` to `members` in the root `Cargo.toml`.
4. **Make the contract suite pass**:
   ```bash
   cargo test -p <your-adapter>
   ```
5. **Document it**: a `CHANGELOG.md` entry (required for any change under `crates/`); a new ADR only if
   the provider's wire is genuinely a new shape, otherwise a note under the existing one.
6. `just preflight`.

## Learn from three worked examples

They are the same pattern at three providers — read whichever is closest to yours:

- [`crates/adapters/shipping-ahamove`](../../crates/adapters/shipping-ahamove) — a courier
  (`ShippingDispatch`), REST, stateful stub. [ADR-0058](../adr/0058-shipping-adapters.md).
- [`crates/adapters/erp-sap`](../../crates/adapters/erp-sap) — an ERP (`ErpSink`), nightly batch
  posting. [ADR-0059](../adr/0059-erp-adapter.md).
- [`crates/adapters/cloud-sync-http`](../../crates/adapters/cloud-sync-http) — the edge→cloud channel,
  stateless request/response. [ADR-0054](../adr/0054-edge-cloud-http-client.md).

## What does *not* go in the adapter

The retry queue, error mailbox, circuit breaker, and latency chart wrap *any* implementation of the
port and live with the dispatch wiring — not inside your adapter. Keep the adapter to request-shaping
and status-mapping. The exact provider endpoint strings and auth are confirmed against the live API in
the gated integration lane, never guessed in the fast unit tests.
