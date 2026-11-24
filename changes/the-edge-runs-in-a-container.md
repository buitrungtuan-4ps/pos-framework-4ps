---
kind: added
---
- **The edge has a container image** (ADR-0113, roadmap-v3 Program C): `deploy/edge-image/` wraps a
  released `pos_edge` binary as one store's container, with its state in one volume, an unprivileged
  uid and a loopback port, and a compose file to run one store by hand. The `edge-image` workflow
  builds it and checks it reads its store's configuration, answers `/healthz` and exits `0` on
  `SIGTERM`. The host agent that would start one per store is not built yet.
