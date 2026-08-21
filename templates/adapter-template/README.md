# `adapter-template` — a starting point for a new port adapter

**Status** Accepted · **Owner** @maintainers-architecture · **Last reviewed** 2026-08-21

This directory is a **scaffold you copy**, not a crate. It is deliberately not a workspace member and
its source files carry a `.tmpl` suffix, so Cargo, `clippy`, `rustfmt`, and `cargo test` never see it —
copy it out, drop the suffix, and fill in the placeholders.

It was extracted from three real adapters that had converged on the same shape — the rule of three
`docs/roadmap.md` P11 calls for:

- `crates/adapters/shipping-ahamove` — a `ShippingDispatch` courier ([ADR-0058](../../docs/adr/0058-shipping-adapters.md))
- `crates/adapters/shipping-grabexpress` — the second courier, built from this template
- `crates/adapters/erp-sap` — an `ErpSink` ([ADR-0059](../../docs/adr/0059-erp-adapter.md))

and the earlier `crates/adapters/cloud-sync-http` ([ADR-0054](../../docs/adr/0054-edge-cloud-http-client.md)),
which drew the transport-seam split first. Read any of those alongside this template — they are the
worked examples the placeholders point at.

## The shape

Every network adapter in this tree is the same three parts, and this template is those three parts
with the specifics blanked out:

1. **A transport seam** (`src/wire.rs.tmpl`). One trait, the only thing that touches a socket, plus a
   production `Tls…Transport` on the tree's pinned rustls/hyper stack ([ADR-0038](../../docs/adr/0038-webhook-tls-sender.md)).
   A provider's API base URL is operator configuration — one fixed, trusted host — so it dials the
   ordinary way, **no SSRF surface** (that guard is only for tenant-supplied destinations, like the
   webhook sender's). The rustls/hyper body is provider-agnostic: copy it verbatim from a sibling
   adapter and only rename the trait and struct.

2. **A pure core** (`src/client.rs.tmpl`). Implements the port. Everything but the one `await` on the
   transport is pure: build the request body, map the provider's vocabulary to the port's value types,
   and — the load-bearing part — map the provider's HTTP status to the right `PortError`. Keep these
   `parse_*` functions free (not methods) so their unit tests need no transport.

3. **A stub-driven contract test** (`tests/contract.rs.tmpl`). A stub transport that speaks the
   provider's exact wire, driving the port's shared contract suite from `pos-contract-tests` with no
   socket. If the provider is stateful (a courier remembers jobs), the stub holds
   `Arc<Mutex<…>>` state and the harness reaches into it for any setup the suite needs.

The real socket path is exercised in the gated integration lane and the soak, never in the fast
pull-request gate — the split every adapter here follows.

## Checklist for a new adapter

1. `cp -r templates/adapter-template crates/adapters/<your-adapter>` and rename each `*.tmpl` to drop
   the suffix (`Cargo.toml.tmpl` → `Cargo.toml`, and so on).
2. Replace every `{{PLACEHOLDER}}` — the crate name, the port, the provider's wire.
3. Add `"crates/adapters/<your-adapter>"` to `members` in the root `Cargo.toml`.
4. Copy the rustls/hyper body of `Tls…Transport::new`/`connect`/`send` verbatim from a sibling
   adapter's `src/wire.rs`; it does not change between providers.
5. Map the provider's statuses to the port's value types and its HTTP codes to `PortError`. Never
   coerce an unknown status to a known one — preserve it (see how the couriers keep an unrecognised
   `Open<ShipmentStatus>` non-terminal).
6. Make the contract suite pass: `cargo test -p <your-adapter>`.
7. If your provider's wire is genuinely new (not a courier or ERP), write an ADR like
   [ADR-0058](../../docs/adr/0058-shipping-adapters.md); if it is another instance of an existing one,
   note it under that ADR instead.
8. Add a `CHANGELOG.md` entry (the `docs-gate` check requires one for any change under `crates/`).
9. Run `just preflight`.

## What does **not** belong in the adapter

The per-adapter queue, retry, error mailbox, and circuit breaker (`docs/roadmap.md` P11) wrap *any*
implementation of the port and live with the dispatch wiring — the way the webhook dispatch task wraps
the webhook sender — not inside the adapter. Keep the adapter to request-shaping and status-mapping.
