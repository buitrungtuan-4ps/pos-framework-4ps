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

mod activation;
mod admin;
mod alerts;
mod apikeys;
mod audit;
mod catalog;
mod config_trees;
mod devices;
mod fleet;
mod floor;
mod order_queue;
mod people;
mod reconcile;
mod registry;
mod rollups;
mod store;
mod subjects;
mod task_health;
mod tax_rates;
mod translations;
mod webhooks;

pub use activation::{ActivationCodeRow, PostgresActivationCodes};
pub use admin::{
    AdminCredentialRow, AdminInviteRow, AdminSessionRow, AdminUserRow, NewSessionRow, PostgresAdmin,
};
pub use alerts::{AlertRow, PostgresAlerts};
pub use apikeys::{ApiKeyRow, ApiKeySummaryRow, PostgresApiKeys};
pub use audit::{AuditLogRow, PostgresAudit};
pub use catalog::{
    CatalogItemRow, CatalogLayoutButtonRow, CatalogMenuRow, CatalogMenuSectionRow,
    CatalogModifierGroupRow, CatalogPlacementRow, CatalogTaxClassRow, CatalogTaxonomyRow,
    PostgresCatalog,
};
pub use config_trees::PostgresConfigTrees;
pub use devices::{DeviceProposalRow, PostgresDeviceProposals};
pub use fleet::{FleetStoreRow, PostgresFleet};
pub use floor::{AreaRow, PostgresFloor, RoutingRuleRow, StationRow, TableRow};
pub use order_queue::{OrderQueueRow, PendingOrderRow, PostgresOrderQueue, PostgresStoreDirectory};
pub use people::{AssignmentRow, EmployeeRow, PostgresPeople, RoleTemplateRow};
pub use reconcile::PostgresReconcile;
pub use registry::{BrandRow, DeviceRow, PostgresRegistry, StoreRow, TenantRow};
pub use rollups::PostgresRollups;
pub use store::{PgTx, PostgresStore};
pub use subjects::{PostgresSubjects, SubjectRow};
pub use task_health::{PostgresTaskHealth, TaskHealthRow};
pub use tax_rates::{PostgresTaxRates, TaxRateRow};
pub use translations::PostgresTranslations;
pub use webhooks::{PostgresWebhooks, WebhookRow, WebhookSummaryRow};
