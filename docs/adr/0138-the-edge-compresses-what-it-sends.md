# ADR-0138 — The edge compresses what it sends to a device

**Status** Accepted · **Owner** @maintainers-architecture · **Date** 2026-09-23
· Adds a feature of an existing dependency (`tower-http`), which `AGENTS.md` §7 counts as a dependency
change · Relates to [ADR-0018](0018-http-websocket-stack.md)

## The problem

`pos_edge` serves the till's JSON and its embedded bundle uncompressed. Measured against a large
store (386 menu items, 40 open tables at peak):

| response | as sent | gzip |
|---|---|---|
| `GET /api/menu` | 149 KB | 7.3 KB |
| `GET /api/orders/live` (peak) | 223 KB | 16.7 KB |
| `GET /api/floor` | 33 KB | 2.3 KB |
| the JS bundle | 176 KB | 50 KB |

The bundle is cached immutably after the first load, so it is paid once per device. The JSON is not:
every reload, every tablet that wakes, every kitchen display switched on mid-service and every
WebSocket `resync` re-reads it — about half a megabyte a time on a large store — over the shop's own
Wi-Fi, which at a busy service is 2.4 GHz shared with every guest's phone.

## Options considered

1. **Leave it.** A LAN is fast until it is a crowded one, and the cost lands on the cheapest tablets at
   the busiest moment.
2. **Pre-compress the bundle at build time only.** Fixes the part that is already cached and leaves
   the part that is not.
3. **Compress responses on the fly with `tower-http`'s `CompressionLayer`, gzip only.**

## Decision

**Option 3**, with these limits:

- **gzip only.** `flate2` is already in the tree, so gzip adds only `async-compression` and its two
  codec crates (MIT/Apache-2.0, both in `deny.toml`'s allow list) and no second version of anything.
  Brotli or zstd would add a codec each for a few percent on JSON that gzip already takes down by 90%.
- **`/api/*` and the embedded assets, never `/ws`.** A WebSocket is not a response body, and the
  fan-out's frames are small and latency-bound.
- **Only above a size floor** (1 KB). A 200-byte refusal gains nothing from a header and a deflate
  stream.
- **Negotiated, as HTTP says.** A client that does not send `Accept-Encoding: gzip` — a print agent, a
  `curl` in a runbook — gets exactly what it got before.

## Consequences accepted

- **Three new crates** in the edge's tree, each a dependency of the one above it. `cargo deny` is the
  check that they stay licensed as stated and single-versioned.
- **CPU on the box.** Deflating a quarter of a megabyte at the default level is on the order of a
  millisecond on the store PCs this targets, against the transfer time it saves on every device.
- **A byte count in a log or a proxy means compressed bytes** for a client that negotiated gzip.
