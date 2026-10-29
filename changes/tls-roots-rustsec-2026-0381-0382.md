---
kind: fixed
---
- **TLS certificate checks take the fixed releases** (RUSTSEC-2026-0382, RUSTSEC-2026-0381).
  `rustls-webpki` moves to 0.103.16, which no longer accepts distrusted intermediates, and
  `webpki-roots` to 1.0.10, which drops the distrusted emSign roots. Both are patch releases of
  crates already in the tree; only `Cargo.lock` changes. `webpki-roots` 0.26.11, reached through
  `async-nats` → `tokio-websockets`, is a re-export of 1.0.10 and has no newer 0.26 release, so
  `deny.toml` ignores that advisory id with a reason and an expiry (2027-01-31, or sooner when
  `async-nats` moves past `tokio-websockets` 0.10).
