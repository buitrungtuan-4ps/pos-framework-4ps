# `printer-escpos`

**Layer** `adapter` · **Owners** @maintainers-architecture

## What it is

Encodes a print document into ESC/POS bytes and writes them through a `Transport` (raw TCP today), applying the port's rules: idempotent by job id, a cash drawer only where the transport allows, a retryable failure when unreachable. It decides nothing about content. `pos-edge` and `pos-print-agent` use it.

## Owns

- `PrinterDriver` over ESC/POS
- The `Transport` seam and the TCP transport

## Depends on / used by

- **Depends on** (workspace crates): `pos-ports`, `pos-proto`; in tests, `pos-contract-tests`, `pos-fakes`.
- **Used by** (workspace crates): `pos-edge`, `pos-print-agent`.

## Entry points

- `src/lib.rs`: `EscPosPrinter`, `Transport`
- `src/escpos.rs`: the encoder
- `src/tcp.rs`: the TCP transport
- `src/device.rs`: device status

## Rules

- [ADR-0026](../../../docs/adr/0026-port-shapes.md) · [ADR-0100](../../../docs/adr/0100-receipt-and-ticket-printing.md) · [ADR-0103](../../../docs/adr/0103-directly-attached-printers.md)

## How to test

```bash
cargo test -p printer-escpos
```
