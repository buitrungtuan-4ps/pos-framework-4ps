# `pos-render`

**Layer** `app` · **Owners** @maintainers-architecture

## What it is

Text rendering for thermal printers. It shapes a line of text in any script (Vietnamese tone marks, Devanagari, Khmer) with `harfrust` and rasterises it into a 1-bit bitmap, for a printer whose code page cannot print it as text. `pos-edge` uses it when it builds a receipt or ticket.

## Owns

- Shaping and rasterising a line of text
- Loading fonts and reporting which scripts they cover

## Depends on / used by

- **Depends on** (workspace crates): `pos-ports`.
- **Used by** (workspace crates): `pos-edge`, `pos-edge-printing`.

## Entry points

- `src/text.rs`: `TextRenderer`, `RenderedLine`
- `src/font.rs`: `FontLibrary`, `ScriptCoverage`
- `src/raster.rs`: `Bitmap`

## Rules

- [ADR-0102](../../docs/adr/0102-printing-any-script.md)

## How to test

```bash
cargo test -p pos-render
```
