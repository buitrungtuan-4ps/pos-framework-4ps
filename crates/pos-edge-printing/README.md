# `pos-edge-printing`

**Layer** `app` · **Owners** @maintainers-architecture

## What it is

The store's printing: which device prints what, the receipt, kitchen ticket and shift-report documents, the cash drawer, and handing each job to a printer directly or to a print agent through the print queue. It stands on `pos-edge-app` (`Edge`, the queue-number and lease authorities, the clock and line notes). It also holds `ValueSource` and `log_in_force`, which say where a setting's value in force comes from (ADR-0160 decision 6), because printing's font size is one of those settings. `pos-edge` re-exports every module under its old path (`pos_edge::printing`, `pos_edge::print_agent`, …) and `ValueSource` and `log_in_force` in `pos_edge::config`, and keeps the print routes and the composition.

## Owns

- `Printers`: device selection, documents, dispatch to a printer or an agent, the drawer kick
- The print agents a store binds (`PrintAgents`), the queue they claim from (`PrintQueue`) and the wake (`PrintWake`), with their SQLite impls
- The words paper prints in each language (`PaperLabels`)
- `ValueSource` and `log_in_force`

## Depends on / used by

- **Depends on** (workspace crates): `pos-core`, `pos-edge-app`, `pos-ports`, `pos-proto`, `pos-render`, `printer-escpos`, `store-sqlite`.
- **Used by** (workspace crates): `pos-edge`.

## Entry points

- `src/printing/mod.rs`: `Printers`, `PrintOutcome`, `DrawerOutcome`; one module per role beside it (`devices.rs`, `documents.rs`, `tickets.rs`, `dispatch.rs`, `drawer.rs`, `agents.rs`, `transports.rs` …)
- `src/print_agent.rs`, `src/print_queue.rs`, `src/print_wake.rs`: the agent lane's seams and their impls
- `src/paper_labels.rs`: `PaperLabels`
- `src/config.rs`: `ValueSource`, `log_in_force`

## Rules

- [ADR-0100](../../docs/adr/0100-receipt-and-ticket-printing.md): receipt and ticket printing
- [ADR-0103](../../docs/adr/0103-directly-attached-printers.md): directly attached printers
- [ADR-0112](../../docs/adr/0112-print-agents.md): print agents
- [ADR-0160](../../docs/adr/0160-everything-a-store-runs-differently-is-published-configuration.md) decision 6: where a setting's value comes from
- [ADR-0170](../../docs/adr/0170-repository-layout-code-naming-and-size-rules.md) decision 4 and [ADR-0175](../../docs/adr/0175-the-crate-list-follows-what-a-move-can-reach.md): why this is a crate
- Each module that logs keeps its old `pos_edge::…` `tracing` target through a `LOG_TARGET` constant (`pos_edge::printing::…`, `pos_edge::print_queue`, `pos_edge::config`), so log filters are unaffected.

## How to test

```bash
cargo test -p pos-edge-printing
```

The print routes' HTTP tests drive the whole router, so they stay in `crates/pos-edge/tests/`.
