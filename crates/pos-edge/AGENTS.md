# `crates/pos-edge` — local rules

Read the root [`AGENTS.md`](../../AGENTS.md) first. This file adds to it and never overrides it.

**What lives here:** the store binary. The async runtime, the `/api/*` routes and WebSocket, the
transaction, and the composition of `pos-core` with the store's adapters. See
[`README.md`](README.md).

## Local rules

- Every command is load → decide (`pos-core`) → apply inside one transaction → publish after the
  commit (`src/app/mod.rs`). A rolled-back transaction prints nothing and tells no device.
- An `/api/*` route, once published, is never removed or renamed
  ([ADR-0111](../../docs/adr/0111-a-second-origin-may-address-the-edge.md)). A new route
  regenerates `docs/snapshots/routes.txt`.
- Every state-changing route says what it needs in `src/http/route_permissions.rs`, or why no
  person acts through it ([ADR-0158](../../docs/adr/0158-the-till-enforces-each-persons-own-permissions.md)).
- A store owns no settings. A value a store may run differently is a published setting
  ([ADR-0160](../../docs/adr/0160-everything-a-store-runs-differently-is-published-configuration.md)).
- The edge keeps selling with no internet. Nothing on the sale path waits on the cloud.
- `src/app/mod.rs` is the busiest file in the repository. Keep changes to it small, and put new
  behaviour in its own module where the area allows.

## Most often touched

`src/app/mod.rs` · `src/printing.rs` · `src/http/mod.rs` and `src/http/<area>.rs` ·
`src/config_client.rs` · `tests/domain_flow.rs` · `tests/acceptance.rs`

## Gates

`cargo test -p pos-edge` (including `routes_snapshot` and the route-permission table) ·
`xtask snapshot` · the `replay` job (`ui/tests/replay.spec.mjs` against `examples/minimal-edge`) ·
`changes/<topic>.md` for a user-visible change (`docs-gate`, [`changes/README.md`](../../changes/README.md)).

## Read first

The [repository map](../../docs/repository-map.md) section on edge routes ·
[`docs/architecture.md`](../../docs/architecture.md) §2 ·
[ADR-0013](../../docs/adr/0013-async-strategy.md) · [ADR-0084](../../docs/adr/0084-device-authentication.md).
