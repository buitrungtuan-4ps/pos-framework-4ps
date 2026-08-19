# `ui/` — the operator interface

**Status** Accepted · **Owner** @maintainers-domain · **Last reviewed** 2026-08-19

The SolidJS + Tailwind app the store devices run. It is built into `ui/dist/` and
**embedded into `pos_edge`** with `rust-embed` ([ADR-0018](../docs/adr/0018-http-websocket-stack.md)),
so a store is one static binary with its interface inside it ([ADR-0002](../docs/adr/0002-one-binary-per-tier.md)).

## Where it stands

The real interface arrives in **P6** ([`docs/roadmap.md`](../docs/roadmap.md)). Until then,
`ui/dist/index.html` is a placeholder that proves the embedding path: `pos_edge` serves it, and the
health endpoint answers, with the network unplugged.

## How the edge serves it

- **Default build** — the contents of `ui/dist/` are compiled into the binary. No files sit next to
  the executable; there is nothing for an operator to lose.
- **`--features dev-ui`** — the edge reads `ui/dist/` from disk instead, so a UI change is a browser
  refresh rather than a Rust rebuild.

When P6 introduces a real front-end toolchain, its build writes to `ui/dist/` and this README gains a
"how to build" section; the Rust side does not change.
