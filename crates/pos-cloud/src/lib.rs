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
//! [`webhook`] pushes the same log outward: a signed, SSRF-guarded cursor per subscribed endpoint.
//! [`config_tree`] is the other direction — the four-level Tenant→Brand→Store→Device configuration
//! the cloud composes, validates, versions, and publishes to each store as a delta or a snapshot.
//! [`auth`] guards the admin surface: an Argon2id password with a mandatory TOTP second factor, and
//! a host-only session cookie, and issues scoped per-tenant API keys for machine callers of `/v1`.
//! [`retention`] is the data-protection cron: it masks personal data in
//! the subject store once it is past its configured retention period (PDPD/GDPR/CCPA), keeping the
//! books reconcilable. [`dashboard`] answers activity dashboards from a materialised rollup a
//! projector keeps current, so a view is an O(days) lookup rather than a log scan.
//!
//! The `/v1` dashboard read is now fully wired: it authenticates a scoped per-tenant API key
//! ([`auth::bearer`]), answers from the materialised rollup ([`dashboard`]) for the key's own
//! tenant, both the rollup table and the API-key table are persisted in `store-postgres`, and the
//! [`dashboard::projector`] background task keeps that rollup current across the fleet
//! (ADR-0036, ADR-0037).
//!
//! Deliberately not here yet, each its own slice (`docs/roadmap.md` P7): the webhook transport's
//! concrete TLS sender and endpoint persistence (ADR-0032), the config tree's persistence and admin
//! routes (ADR-0033), the super-admin login route + credential persistence and the API-key
//! provisioning route (ADR-0034, ADR-0037), and the subject-store schema and the retention runner's
//! wiring into `main` (ADR-0035).

#![forbid(unsafe_code)]

pub mod auth;
pub mod clock;
pub mod cloud;
pub mod config;
pub mod config_tree;
pub mod cursor;
pub mod dashboard;
pub mod http;
mod openapi;
mod persistence;
pub mod retention;
pub mod webhook;

pub use cloud::{Cloud, DailyRollup, IngestOutcome};
pub use config::{CloudConfig, NatsIngestConfig};
