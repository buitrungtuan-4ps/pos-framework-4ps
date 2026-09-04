# POS Framework

**Status** Accepted · **Owner** @maintainers-architecture · **Last reviewed** 2026-08-18

A multi-country, offline-first Point-of-Sale framework written in Rust.
One store runs entirely on its own hardware. The cloud manages configuration, fleet operations, and reporting — it is never in the path of a sale.

**Status:** design frozen, implementation under way — see [docs/roadmap.md](docs/roadmap.md). Proprietary, internal use (see [LICENSE](LICENSE)).

---

## Core promises

| Promise | How it is delivered |
|---|---|
| **A store never stops selling** | All sales logic runs on a local binary with a local database. Network loss changes nothing except cloud-only features. |
| **Plug and play** | Fork the repo, set 4–6 GitHub Secrets, click *Run workflow*. ~15 minutes later you have a running cloud and an admin UI. |
| **Light and fast** | Two static Rust binaries. A store server idles below 1% CPU; a touch-to-persist operation costs 1–4 ms. |
| **Multi-country** | Country-specific obligations (tax invoices, locale, vendors) live in `countries/<cc>/` and are selected by a Cargo feature. The core never changes when you add a country, and a fork that needs two countries out of five compiles only two — see [ADR-0027](docs/adr/0027-country-modules.md). |
| **Cross-platform** | Store server runs on Windows and Linux. Clients are browsers: POS terminals, tablets, phones, kitchen displays. |
| **Easy to maintain** | One monorepo, one CI, machine-enforced rules. Upgrades and rollbacks are the same button. |

## Architecture at a glance

```
   ┌──────────────────────── STORE (LAN, fully autonomous) ────────────────────────┐
   │  POS terminal ─┐                                                              │
   │  Tablet      ──┼─► pos_edge (single binary)  ──► SQLite (WAL)                 │
   │  Phone       ──┤     • sales domain               • orders, bills, shifts     │
   │  Kitchen KDS ──┘     • embedded web UI            • outbox (unsent events)    │
   │                      • ESC/POS printing                                       │
   └───────────────────────────────┬───────────────────────────────────────────────┘
                                   │ outbound only (NATS JetStream over TLS)
                                   ▼
   ┌──────────────────────────── CLOUD (single VPS) ───────────────────────────────┐
   │  pos_cloud (single binary)   PostgreSQL      NATS       Garage (S3)           │
   │  • admin dashboard           • partitioned   • durable  • backups             │
   │  • config tree + hot reload    by store        queue    • OTA artifacts       │
   │  • fleet / OTA rings         • RLS per tenant                                 │
   │  • public API + webhooks     • rollup tables                                  │
   │  • QR ordering (guest web)                                                    │
   └───────────────────────────────────────────────────────────────────────────────┘
```

## Repository layout

```
pos-framework/
├── AGENTS.md                  Read first. Rules for humans and AI agents.
├── CONTRIBUTING.md            How to work in this repo.
├── justfile                   just preflight | build | test | sign | deploy
├── deny.toml                  License + advisory policy (blocks copyleft)
├── rust-toolchain.toml        Pinned toolchain
├── crates/
│   ├── pos-core/              Pure domain. No tokio, no sqlx, no I/O.
│   ├── pos-ports/             Trait definitions (the framework's boundaries)
│   ├── pos-proto/             Wire types, events, PROTOCOL_VERSION
│   ├── adapters/
│   │   ├── store-sqlite/      EventStore, ConfigStore (edge)
│   │   ├── store-postgres/    EventStore, ConfigStore (cloud)
│   │   ├── link-nats/         MessageLink
│   │   ├── printer-escpos/    PrinterDriver
│   │   ├── payment-*/         PaymentTerminal per acquirer
│   │   ├── vendor-*/          DeliveryVendor per marketplace
│   │   └── shipping-*/        ShippingDispatch per courier
│   ├── pos-country/           What a country module is; the feature-selected registry
│   ├── pos-contract-tests/    The suite every implementation of every port must pass
│   ├── pos-fakes/             In-memory implementations; passes every suite
│   ├── pos-edge/              Store binary: wires adapters into core
│   ├── pos-cloud/             Cloud binary: wires adapters into core
│   └── pos-simulator/         Virtual fleet for load and OTA testing
├── countries/                 One directory per country. Add or remove yours here.
│   ├── zz/                    Reference module — copy this to start a country
│   └── vn/                    Vietnam (arrives in P10)
├── ui/                        SolidJS + Tailwind, embedded into binaries
├── deploy/                    compose.yml, Caddyfile.d/, bootstrap.sh, k8s/
├── examples/                  Runnable examples (built by CI)
├── templates/adapter-template/   Extracted at the *third* adapter, not before (rule of three)
└── docs/                      See map below
```

## Quickstart

**Run the store on your laptop** — no database, no hardware, no cloud:

```bash
just run-edge      # or: cargo run -p minimal-edge      →  open http://127.0.0.1:8787/
```

**Deploy the cloud** to one VPS, with no command typed on the server: fork, set a handful of GitHub
Actions secrets, and run the **deploy** workflow. The exact secrets and ordered steps are the single
source of truth in the [deploy runbook](docs/deploy-runbook.md).

New here, or picking a task? The four short [**guides**](docs/guides/) each finish in one sitting:
[start from zero](docs/guides/start-from-zero.md) · [write an adapter](docs/guides/write-an-adapter.md) ·
[add a country](docs/guides/add-a-country-module.md) · [run the simulator](docs/guides/run-the-simulator.md).

## Documentation map

| Document | Read it when you need to know |
|---|---|
| [docs/guides/](docs/guides/) | Task-shaped how-tos: start from zero, write an adapter, add a country, run the simulator |
| [docs/deploy-runbook.md](docs/deploy-runbook.md) | Fork → set secrets → live admin UI: the deployment checklist and every secret |
| [docs/go-live.md](docs/go-live.md) | Fork → trading store, in order, with every point the sequence stops for a human |
| [docs/gate-register.md](docs/gate-register.md) | The gates only a person, a credential, real hardware or an outside body can clear |
| [AGENTS.md](AGENTS.md) | The rules. **Read before writing any code.** |
| [docs/architecture.md](docs/architecture.md) | How the system is built and why |
| [docs/pos-spec.md](docs/pos-spec.md) | What the product does (business behaviour) |
| [docs/naming-and-api.md](docs/naming-and-api.md) | How to name anything: JSON, DB, events, permissions |
| [docs/ui-ux.md](docs/ui-ux.md) | How screens must behave |
| [docs/engineering-guide.md](docs/engineering-guide.md) | Branching, CI, releases, deployment, AI contributions |
| [docs/roadmap.md](docs/roadmap.md) | What is being built next, in what order, and the exit criterion for each phase |
| [docs/capacity-and-reliability.md](docs/capacity-and-reliability.md) | Sizing numbers, load limits, failure and recovery matrix |
| [docs/design-principles.md](docs/design-principles.md) | SOLID, KISS, DRY, YAGNI as checkable rules for this codebase |
| [docs/glossary.md](docs/glossary.md) | Restaurant and framework vocabulary (86, fire, bump, comp, business date) |
| [docs/adr/](docs/adr/) | The twelve decisions that shaped the system, and why |
| [CONTRIBUTING.md](CONTRIBUTING.md) | Workflow: orientation, verification, pull requests, changelog |
| [CHANGELOG.md](CHANGELOG.md) | What changed, when, and what to do about it when upgrading |

> **Canonical language:** the English documents in this repository are the single source of truth. Earlier Vietnamese design documents are a frozen archive of the design discussion and are not maintained; never reconcile code against them.

### Also in the repository

[`CONTRIBUTING.md`](CONTRIBUTING.md) — orientation, build and verify commands, PR rules · [`docs/design-principles.md`](docs/design-principles.md) — SOLID/KISS/DRY/YAGNI as concrete rules · [`docs/glossary.md`](docs/glossary.md) — vocabulary · [`docs/adr/`](docs/adr/) — 12 architecture decision records · [`CHANGELOG.md`](CHANGELOG.md) — version history.

## Non-goals

This framework deliberately does **not** include: payment processing (we integrate terminals, we are not an acquirer), CRM and loyalty, reservations and waitlists, payroll, full purchase-order management, or a training mode. See [`docs/pos-spec.md`](docs/pos-spec.md) §Scope for the reasoning behind each exclusion.
