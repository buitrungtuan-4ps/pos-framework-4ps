# `pos-core`

**Layer** `core` · **Owners** @maintainers-domain

## What it is

The domain: every rule that decides what a sale means. Each command is `decide(state, command, ctx) -> Result<Decision, DomainError>`: state arrives loaded, and the decision carries the events to append and the effects to fire after commit. The caller owns the transaction. It performs no I/O, awaits nothing, and does not depend on `pos-ports`.

## Owns

- Orders, bills, shifts and tables as explicit state machines
- The permission catalogue (`docs/permissions.md`) and capability flags
- Pricing, promotions, fees, inventory and availability rules
- The OTA rollout decision

## Depends on / used by

- **Depends on** (workspace crates): `pos-proto`.
- **Used by** (workspace crates): `pos-cloud`, `pos-cloud-catalog`, `pos-cloud-config`, `pos-cloud-devices`, `pos-cloud-floor`, `pos-cloud-people`, `pos-contract-tests`, `pos-edge`, `pos-edge-app`, `pos-edge-printing`, `pos-simulator`.

## Entry points

- `src/decision.rs`: `decide_table`, `decide_line`, `decide_bill`, `decide_shift` and the decision context
- `src/permission.rs`: the `permissions!` catalogue
- `src/capability.rs`: store-profile capability flags
- `src/machines.rs`: the state-machine tables behind `docs/state-machines.md`

## Rules

- [ADR-0006](../../docs/adr/0006-ports-and-adapters.md) · [ADR-0013](../../docs/adr/0013-async-strategy.md) · [ADR-0028](../../docs/adr/0028-settlement-and-payment-invariant.md) · [ADR-0029](../../docs/adr/0029-append-command-merge-semantics.md) · [ADR-0158](../../docs/adr/0158-the-till-enforces-each-persons-own-permissions.md) · [ADR-0159](../../docs/adr/0159-a-fee-is-configuration.md)

## How to test

```bash
cargo test -p pos-core
POS_UPDATE_SNAPSHOTS=1 cargo test -p pos-core    # regenerate permissions, capabilities, state machines
```
