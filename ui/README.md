# `ui/` — the operator interface

**Status** Accepted · **Owner** @maintainers-domain · **Last reviewed** 2026-08-19

The SolidJS + Tailwind app the store devices run. It is built into `ui/dist/` and
**embedded into `pos_edge`** with `rust-embed` ([ADR-0018](../docs/adr/0018-http-websocket-stack.md)),
so a store is one static binary with its interface inside it ([ADR-0002](../docs/adr/0002-one-binary-per-tier.md)).

## Where it stands

The real interface arrives in **P6** ([`docs/roadmap.md`](../docs/roadmap.md)). `ui/dist/` is build
output and is gitignored, so a fresh checkout has nothing to embed. Until P6's toolchain populates
it, `pos-edge`'s `build.rs` writes a placeholder `ui/dist/index.html` when one is absent — and never
overwrites a real build. That placeholder proves the embedding path: `pos_edge` serves it, and the
health endpoint answers, with the network unplugged.

## How the edge serves it

- **Default build** — the contents of `ui/dist/` are compiled into the binary. No files sit next to
  the executable; there is nothing for an operator to lose.
- **`--features dev-ui`** — the edge reads `ui/dist/` from disk instead, so a UI change is a browser
  refresh rather than a Rust rebuild.

## How to build

The app is SolidJS + Tailwind, built with Vite and type-checked with TypeScript. Node ≥ 22 and
`pnpm`:

```sh
cd ui
pnpm install          # once, and after dependency changes
pnpm build            # tsc --noEmit, then vite build → ui/dist/
pnpm dev              # live server, proxying /api and /ws to a store on :8787
pnpm typecheck        # types only
```

`pnpm build` writes `ui/dist/`, which the default `pos-edge` build embeds. For a live UI against a
running store, `pnpm dev` (Vite) serves the app and proxies the API and the WebSocket to
`127.0.0.1:8787`, so a UI change is a browser refresh; `cargo run -p pos-edge --features dev-ui`
reads `ui/dist/` from disk instead of embedding it. CI type-checks and builds the app on every pull
request (the `ui` job); it does not embed the bundle, so the Rust build still uses the placeholder on
a fresh checkout.

## Layout

- `src/styles/tokens.css` — the design tokens (spacing, type, touch, radius, motion, colour), the one
  source the interface is built from; light and dark, KDS defaulting to dark.
- `src/lib/` — money (integer minor units, never a float) and the stand-in menu.
- `src/api/` — the typed client for the edge's routes and the reconnecting `/ws` live link.
- `src/state/` — the client projection folded from the fan-out.
- `src/screens/` — the primary flow: floor → order → pay. The remaining screens (KDS, expo, Today,
  shift, pairing) follow.
