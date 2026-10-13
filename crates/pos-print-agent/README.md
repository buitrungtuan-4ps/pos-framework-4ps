# `pos-print-agent`

**Layer** `app` · **Owners** @maintainers-architecture

## What it is

A small binary, `pos_print_agent`, for a device whose transport reaches a printer the edge cannot. It claims rendered jobs from the store's edge, writes the bytes to the printer, and acknowledges them. It decides nothing: no rendering, no domain, no configuration beyond where the edge is. It persists one value per printer, the id of the last job it wrote.

## Owns

- Claiming and acknowledging jobs over the edge's print-job routes
- Writing to the printer through `printer-escpos`
- The last-written job id, so a redelivered job is not printed twice

## Depends on / used by

- **Depends on** (workspace crates): `pos-ports`, `pos-proto`, `printer-escpos`.
- **Used by** (workspace crates): no workspace crate.

## Entry points

- `src/main.rs`: the binary
- `src/lib.rs`: `Config`, `LeasedJob`, `EdgeTransport`, `Printing`, `LastWritten`
- `src/printers.rs`: the printers it writes to
- `src/wire.rs`: the edge's job wire format

## Rules

- [ADR-0112](../../docs/adr/0112-print-agents.md) · [ADR-0113](../../docs/adr/0113-the-host-agent.md)

## How to test

```bash
cargo test -p pos-print-agent
cargo run -q -p xtask -- print-agent-deps    # it links only the crates ADR-0112 allows
```
