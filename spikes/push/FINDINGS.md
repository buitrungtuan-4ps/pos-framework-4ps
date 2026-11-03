# Spike: push to the store (throwaway, never merged)

This measures publish → edge-apply latency if the cloud publishes a "config changed" wake on
`pos.store.<store_id>.config` (ADR-0195) over core NATS.

**Setup.** A real local `nats-server` 2.11.9 (built with `go install` through the Go module proxy).
`async-nats` 0.50 is the same version link-nats pins. The test is
`crates/pos-edge/tests/spike_wake.rs`, run with `cargo test -p pos-edge --release --test spike_wake
-- --ignored`. The edge side:
- a real `Edge` over the in-memory store;
- the real `ConfigClient::pump_once` on each wake;
- a simulated cloud transport that sleeps a configurable round trip and parses the document from a
  JSON string (a fake menu of N items).

A till subscribed to the edge fan-out stamps the `config_applied` frame. There are 200 wakes per
row, on a 4-core Xeon container. The broker, the "cloud" and the edge are all on localhost.

| Document | Simulated cloud RTT | Wake (publish+flush → received) p50 / p95 | Pull+apply p50 / p95 | Publish → applied p50 / p95 | Publish → till `config_applied` p50 / p95 |
|---|---|---|---|---|---|
| 1 item (<1 KB) | 0 ms | 0.38 / 0.70 | 1.20 / 1.33 | 1.60 / 2.01 | 1.67 / 2.12 |
| 300 items (64 KB) | 0 ms | 0.42 / 0.68 | 2.52 / 3.27 | 2.96 / 4.04 | 3.09 / 4.21 |
| 1000 items (215 KB) | 0 ms | 0.44 / 0.81 | 5.80 / 8.17 | 6.31 / 9.33 | 6.41 / 9.48 |
| 300 items | 40 ms | 0.51 / 0.92 | 42.9 / 44.2 | 43.4 / 45.1 | 43.6 / 45.4 |
| 300 items | 120 ms | 0.52 / 1.33 | 123.1 / 125.3 | 123.7 / 126.1 | 123.8 / 126.3 |

**What it says**
- **The wake itself costs under 1 ms.** Applying is 1–8 ms for a document up to 215 KB (JSON parse
  plus session rebuild plus the fan-out). The pull's round trip to the cloud dominates. A store in
  Vietnam on a cloud in the region (20–60 ms RTT, plus TLS on a cold connection) lands well inside
  ADR-0195's "about 1 s". The wake-to-pull design costs exactly one extra round trip over shipping
  the delta, which is 1–2 % of the budget.
- The real figures add: the wake's broker hop over the internet (one RTT to the broker, the same
  order as the pull); HTTP+TLS on `/sync` (keep-alive matters, since a cold TLS handshake adds 1–2
  RTT); the cloud's own config read; and the till's re-render after `config_applied`. The re-render
  is a till-side re-read of the changed nodes, which the till-latency baseline puts at tens of ms
  for a full order screen at 1×.

**What is needed for the real thing (ADR-0195), found while wiring it**
- `ConfigClient::pump_once` is safe to call from a second task, and this spike did exactly that.
  However, nothing makes it single-flight against the 30 s `run` loop. ADR-0195's "at most one pull
  at a time, wakes collapse" needs a guard (a `Semaphore(1)` plus a pending flag) inside the client
  or around it.
- `async-nats` `subscribe` on the connection the edge already opens works with no new
  dependency in pos-edge, because link-nats has it. For the spike it was added as a pos-edge
  dev-dependency, so the real code goes through the new `StoreWake` port in link-nats.
- On reconnect, async-nats re-subscribes on its own. ADR-0195's "pull once on every (re)connect"
  needs the connection's event callback (`ConnectOptions::event_callback`, `Event::Connected`),
  which link-nats does not wire today.
- The broker's one fleet token lets any store publish any wake (ADR-0195 accepts this: a forged
  wake costs one `up_to_date` pull). Rate limiting to 1 pull/s is the defence until per-store mTLS.
- **Spike artefact:** the panics after each row are the previous row's subscriber task sending into
  a closed channel at teardown. They are not part of the measurement.
- A real `pos_cloud` (Postgres) was not run. The pull is simulated, so the RTT rows are the honest
  stand-in for the network.
