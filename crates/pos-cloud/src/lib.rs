// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The cloud binary's library: idempotent ingest and the rollup read model (P7).
//!
//! `pos_cloud` receives every store's events, stores them idempotently in PostgreSQL
//! ([ADR-0016](../../../docs/adr/0016-postgres-access.md), [ADR-0022](../../../docs/adr/0022-events-partition-strategy.md)),
//! and answers dashboards from rollups derived from that log. The spine is [`Cloud::ingest`] and
//! [`Cloud::daily_rollups`], generic over the [`EventStore`](pos_ports::event_store::EventStore) so
//! the whole thing runs against the in-memory fake in tests and `store-postgres` in the cloud
//! (ADR-0026). Events reach ingest two ways: the **[`cursor`]** — a durable NATS consumer that is the
//! production feed — and the `/internal/ingest` route the nightly reconciliation re-pushes through.
//! The public read surface is the generated **`/v1`** API and its OpenAPI document ([`http`]).
//!
//! Deliberately not here yet, each its own slice (`docs/roadmap.md` P7): webhooks (a cursor over the
//! log with HMAC and SSRF protection), super-admin auth (Argon2 + TOTP) and per-tenant API keys, the
//! four-level config tree, the retention/PII-masking cron, and the dashboard screens with
//! materialised rollups.

#![forbid(unsafe_code)]

pub mod cloud;
pub mod config;
pub mod cursor;
pub mod http;
mod openapi;

pub use cloud::{Cloud, DailyRollup, IngestOutcome};
pub use config::{CloudConfig, NatsIngestConfig};
