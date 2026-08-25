# Guides

**Status** Accepted · **Owner** @maintainers-architecture · **Last reviewed** 2026-08-21

Five short, task-shaped guides. Each is meant to be finished in one sitting.

| I want to… | Guide |
|---|---|
| Clone the repo and see a sale happen | [Start from zero](start-from-zero.md) |
| Deploy the cloud to a real server | [Start from zero → Part 2](start-from-zero.md#part-2--deploy-the-cloud-to-a-vps-15-minutes), then the [deploy runbook](../deploy-runbook.md) |
| Bring a real store online from the dashboard | [Bring a store online](bring-a-store-online.md) |
| Write an adapter for a new vendor/device | [Write an adapter](write-an-adapter.md) |
| Add a new country (tax, locale, vendors) | [Add a country module](add-a-country-module.md) |
| Prove the capacity numbers, run fleet scenarios | [Run the simulator](run-the-simulator.md) |

## The one thing to understand first: there are two tiers

This is not one server. It is two programs, and they run in **different places**:

```
  STORE (in the shop)                         CLOUD (one VPS)
  ┌───────────────────────────┐               ┌────────────────────────────┐
  │ pos_edge                  │  events ───►  │ pos_cloud                  │
  │ • the actual selling      │  (outbound    │ • admin + dashboards       │
  │ • SQLite, ESC/POS printer │   only)       │ • config, fleet/OTA        │
  │ • browsers on the LAN     │  ◄─── config  │ • public API, QR ordering  │
  │ • SELLS WITH NO INTERNET  │   (store pulls)│ • never in a sale's path   │
  └───────────────────────────┘               └────────────────────────────┘
```

- **`pos_edge`** is where money is made. It runs on a machine **in the shop** (a mini-PC, Windows or
  Linux) and keeps selling even when the internet is down. It is **not** deployed to the cloud.
- **`pos_cloud`** is the back office. It runs on **one VPS** and manages configuration, fleet
  operations, reporting, the public API, and QR ordering. If it goes down, stores keep selling.

So "deploy to the cloud" brings up `pos_cloud` only. To try the selling side, run `pos_edge`
locally — that is Part 1 of [Start from zero](start-from-zero.md).
