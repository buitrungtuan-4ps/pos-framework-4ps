// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The cloud [`EventStore`](pos_ports::event_store::EventStore) over PostgreSQL (P7).
//!
//! `tokio-postgres` behind a `deadpool` pool, hand-written SQL, no build-time database
//! ([ADR-0016](../../../docs/adr/0016-postgres-access.md)). The event log is range-partitioned
//! monthly on `business_date` and idempotent by `event_id`
//! ([ADR-0022](../../../docs/adr/0022-events-partition-strategy.md)); tenant isolation is row-level
//! security on `tenant_id` ([ADR-0008](../../../docs/adr/0008-postgres-partitioning.md)), orthogonal
//! to the partition key, so a query that forgets the tenant sees nothing.
//!
//! Correctness is a test obligation, not a compiler one: `store-postgres` runs the shared port
//! contract suite ([ADR-0026](../../../docs/adr/0026-port-shapes.md)) and cloud-specific RLS and
//! partition-routing tests **against a real PostgreSQL**. Those tests are gated behind the
//! `integration` Cargo feature (off by default), so the pull-request build neither compiles nor
//! runs them; the merge-to-`main` integration job turns the feature on against a pinned
//! `postgres:16` service, with `DATABASE_URL` pointing at it.

#![forbid(unsafe_code)]

mod admin;
mod apikeys;
mod config_trees;
mod reconcile;
mod rollups;
mod store;
mod subjects;
mod webhooks;

pub use admin::{AdminCredentialRow, PostgresAdmin};
pub use apikeys::{ApiKeyRow, ApiKeySummaryRow, PostgresApiKeys};
pub use config_trees::PostgresConfigTrees;
pub use reconcile::PostgresReconcile;
pub use rollups::PostgresRollups;
pub use store::{PgTx, PostgresStore};
pub use subjects::{PostgresSubjects, SubjectRow};
pub use webhooks::{PostgresWebhooks, WebhookRow, WebhookSummaryRow};
