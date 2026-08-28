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
//! (ADR-0036, ADR-0037). The super-admin surface is wired too: [`auth::admin`] loads the credential
//! from `store-postgres`, `POST /admin/login` runs the two-factor check and issues a host-only
//! session cookie backed by a server-side session table, and [`http`] exposes the logout and
//! session-guard routes plus — behind that guard — the scoped per-tenant API-key provisioning routes
//! (`/admin/api-keys`: issue once, list, revoke) (ADR-0034, ADR-0037).
//!
//! The config tree ([`config_tree`]) now persists and is authored over `/admin`: a `ConfigTreeStore`
//! seam and a `store-postgres` table round-trip a store's whole tree, and the `/admin/stores/{id}/config`
//! routes publish a level (validated, versioned) and read the effective document (ADR-0033).
//! [`retention`] is wired too: the `SubjectStore` seam is backed by a `store-postgres` `subjects`
//! table, and the daily masking runner starts in `main` whenever a retention period is configured
//! (ADR-0035). Webhook endpoints now persist: the [`webhook::store`] `WebhookEndpointStore` seam is
//! backed by a `store-postgres` `webhook_endpoints` table that holds a subscription's durable facts —
//! destination, signing secret, cursor, disabled flag — so the admin CRUD lists a tenant's endpoints
//! and the delivery task can reload the enabled fleet across a restart (ADR-0032).
//!
//! First-boot super-admin enrolment now exists too: [`auth::enrol`] and the token-gated
//! `/admin/setup` route provision the single credential [ADR-0034](../../../docs/adr/0034-super-admin-auth.md)
//! always assumed but never wrote, keyed on a one-time setup token the deploy bootstrap mints
//! ([ADR-0045](../../../docs/adr/0045-first-boot-admin-enrolment.md), P8).
//!
//! Deliberately not here yet, its own slice: the subject-store *writer* that populates personal data
//! (P10/P11 marketplace and corporate-invoice buyer fields, ADR-0035).

#![forbid(unsafe_code)]

pub mod activation;
pub mod alerts;
pub mod assets;
pub mod audit;
pub mod auth;
pub mod campaigns;
pub mod catalog;
pub mod catalog_compiler;
pub mod clock;
pub mod cloud;
pub mod config;
pub mod config_tree;
pub mod countries;
pub mod cursor;
pub mod dashboard;
pub mod devices;
pub mod export;
pub mod fleet;
pub mod floor_compiler;
pub mod floorplan;
pub mod health;
pub mod http;
pub mod images;
pub mod import;
pub mod inventory;
pub mod media;
pub mod metrics;
mod openapi;
pub mod orders;
pub mod people;
pub mod people_compiler;
mod persistence;
pub mod qr;
pub mod qr_http;
pub mod reconcile;
pub mod registry;
pub mod relay;
pub mod retention;
pub mod scheduling;
pub mod tax;
pub mod translations;
pub mod vouchers;
pub mod webhook;

pub use cloud::{Cloud, DailyRollup, IngestOutcome};
pub use config::{CloudConfig, NatsIngestConfig};
