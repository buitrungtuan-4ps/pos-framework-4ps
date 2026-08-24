# Glossary

**Status** Accepted · **Owner** @maintainers-domain · **Last reviewed** 2026-08-18

| Term | Meaning |
|---|---|
| **Tenant → Brand → Store** | Three-level ownership tree. Configuration inherits downwards and may be overridden at each level |
| **Edge** | The in-store runtime (`pos_edge`) and the machine it runs on |
| **Cloud** | The control plane (`pos_cloud`) plus PostgreSQL, NATS and object storage |
| **Cell** | One complete, independent cloud deployment serving one country |
| **Outbox** | Durable local queue of domain events awaiting upload, written in the same transaction as the state change |
| **Lease** | Cloud-issued token proving that a given machine is the single active server for a store; prevents split-brain and duplicate receipt numbers |
| **Ring** | A batch of stores in a staged rollout: canary → wider → fleet |
| **Port / Adapter** | A trait we own / an implementation that talks to one specific external system |
| **Contract test** | A shared test suite that every implementation of a port must pass |
| **86** | Marking an item unavailable, immediately on every device and delivery channel |
| **Course** | A group of items intended to reach the table together |
| **Fire** | Sending items to the kitchen. Inventory is consumed at fire time, not at payment |
| **Bump** | Marking a kitchen ticket complete on the KDS |
| **Expo** | The pass; a screen aggregating bumped items per table for runners |
| **Comp** | An item given free of charge — distinct from a discount (price reduction) and a void (never happened) |
| **BOM** | Bill of materials: the ingredient recipe of a menu item, including per-modifier deltas |
| **Available-to-make** | Sellable quantity of a dish derived from shared ingredient stock: `floor(min(stock[i] / recipe[i]))` |
| **Business date** | The trading day a transaction belongs to, using the store's cut-off (default 04:00) rather than the calendar date |
| **Blind close** | Cash count entered before the system reveals the expected amount |
| **Split item** | A single line divided between two variants, such as a half-and-half pizza, with a configurable pricing rule |
| **Capability flag** | A boolean in the configuration tree that switches a product behaviour on or off, for example `tables_enabled` |
| **Cursor feed** | `GET /v1/events` — pull-based event delivery where the consumer keeps its own position |
| **Rollup** | Pre-aggregated table maintained on ingest so dashboards never scan raw events |
| **Fiscalization** | The port and country crates implementing legal invoicing obligations |
| **Subject id** | Reference from an event to a record in the separate personal-data store, so erasure never rewrites the event log |
