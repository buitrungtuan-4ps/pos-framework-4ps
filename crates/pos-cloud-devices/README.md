# `pos-cloud-devices`

**Layer** `app` · **Owners** @maintainers-cloud

## What it is

The cloud's devices: how a box first earns a credential, and the devices a store runs. It holds the once-only activation-code exchange, the claim of a box installed with no store, the device proposals a store makes and the console approves, the store's `devices` node and its revocation deny-list, and their routes and stores. It stands on `pos-cloud-api` (authentication, the device credential, the audit trail and the refusals) and `pos-cloud-config` (the publish every area writes through). `pos-cloud` re-exports `activation`, `claim` and `devices` under their old paths, mounts the routers, and keeps the admitted-device roster, which reads the reports' rollups through its application state.

## Owns

- The activation-code exchange and the `ActivationCodeStore` trait
- Claiming an unclaimed box and the `ClaimStore` trait
- Device proposals, approval, printers and terminals, and the `DeviceProposalStore` trait
- The compile of a store's approved devices to its `devices` node, and the revoked-device deny-list
- Their routes: `/activate`, `/claim`, `/sync/.../devices`, `/admin/devices/*`, the devices publish and the revoke
- Its stores over PostgreSQL: the claim, activation-code and device-proposal tables, through `store-postgres`

## Depends on / used by

- **Depends on** (workspace crates): `pos-cloud-api`, `pos-cloud-config`, `pos-core`, `pos-proto`, `store-postgres`.
- **Used by** (workspace crates): `pos-cloud`.

## Entry points

- `src/activation.rs`: `ActivationCodeStore`, `hash_code`
- `src/claim.rs`: `ClaimStore`, `UserCode`
- `src/devices.rs`: `DeviceProposalStore`, `DeviceProposalSummary`
- `src/routes.rs`: `device_router`; `src/routes/`: `activation_router`, `claim_router`, `device_publish_router`, `device_revoke_router`
- `src/persistence.rs`: the store-postgres impls

## Rules

- [ADR-0041](../../docs/adr/0041-device-onboarding.md): device onboarding
- [ADR-0050](../../docs/adr/0050-activation-code-exchange.md): the activation-code exchange
- [ADR-0118](../../docs/adr/0118-one-credential-per-box-and-the-cloud-learns.md): one credential per box, and the revocation deny-list
- [ADR-0148](../../docs/adr/0148-an-unclaimed-box-shows-a-code-and-the-console-claims-it.md): claiming an unclaimed box
- [ADR-0170](../../docs/adr/0170-repository-layout-code-naming-and-size-rules.md) decision 4: why this is a crate

## How to test

```bash
cargo test -p pos-cloud-devices
```

The routes' HTTP tests drive the whole router, so they stay in `crates/pos-cloud/tests/`.
