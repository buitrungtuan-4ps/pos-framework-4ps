// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The cloud binary's library: idempotent ingest and the rollup read model (P7).
//!
//! `pos_cloud` receives every store's events, stores them idempotently in PostgreSQL
//! ([ADR-0016](../../../docs/adr/0016-postgres-access.md), [ADR-0022](../../../docs/adr/0022-events-partition-strategy.md)),
//! and answers dashboards from rollups derived from that log. This first slice is the **ingest →
//! rollup spine**: [`Cloud::ingest`] and [`Cloud::daily_rollups`], generic over the
//! [`EventStore`](pos_ports::event_store::EventStore) so the whole thing runs against the in-memory
//! fake in tests and `store-postgres` in the cloud (ADR-0026).
//!
//! Deliberately not here yet, each its own slice (`docs/roadmap.md` P7): the public `/v1` API and
//! its generated OpenAPI (ADR-0019), the NATS cursor consumer that drives ingest in production,
//! webhooks (a cursor over the log with HMAC and SSRF protection), super-admin auth (Argon2 +
//! TOTP) and per-tenant API keys, the four-level config tree, the retention/PII-masking cron, and
//! the dashboard screens with materialised rollups.

#![forbid(unsafe_code)]

pub mod cloud;
pub mod config;
pub mod http;

pub use cloud::{Cloud, DailyRollup, IngestOutcome};
pub use config::CloudConfig;
