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
mod campaigns;
mod catalog;
mod config_trees;
mod devices;
mod fleet;
mod floor;
mod inventory;
mod media;
mod order_queue;
mod ota;
mod people;
mod reconcile;
mod registry;
mod rollups;
mod scheduling;
mod store;
mod subjects;
mod task_health;
mod tax_rates;
mod translations;
mod vouchers;
mod webhooks;

pub use activation::{ActivationCodeRow, PostgresActivationCodes};
pub use admin::{
    AdminCredentialRow, AdminInviteRow, AdminSessionRow, AdminUserRow, NewSessionRow, PostgresAdmin,
};
pub use alerts::{AlertRow, PostgresAlerts};
pub use apikeys::{ApiKeyRow, ApiKeySummaryRow, PostgresApiKeys};
pub use audit::{AuditLogRow, AuditOrder, PostgresAudit};
pub use campaigns::{CampaignRow, PostgresCampaigns};
pub use catalog::{
    CatalogItemRow, CatalogLayoutButtonRow, CatalogMenuRow, CatalogMenuSectionRow,
    CatalogModifierGroupRow, CatalogPlacementRow, CatalogTaxClassRow, CatalogTaxonomyRow,
    ItemOrder, PostgresCatalog,
};
pub use config_trees::{
    BumpOutcome, PostgresConfigTrees, StoredBump, StoredRegionWrite, StoredRetire, StoredSettle,
};
pub use devices::{DeviceProposalRow, PostgresDeviceProposals};
pub use fleet::{FleetStoreRow, PostgresFleet};
pub use floor::{AreaRow, PostgresFloor, RoutingRuleRow, StationRow, TableRow};
pub use inventory::{InventoryRow, PostgresInventory};
pub use media::{MediaAssetRow, PostgresMedia};
pub use order_queue::{OrderQueueRow, PendingOrderRow, PostgresOrderQueue, PostgresStoreDirectory};
pub use ota::{PostgresReleases, ReleaseArtifactRow};
pub use people::{AssignmentRow, EmployeeOrder, EmployeeRow, PostgresPeople, RoleTemplateRow};
pub use reconcile::{PostgresReconcile, ReconcileRunRow};
pub use registry::{BrandRow, DeviceRow, PostgresRegistry, StoreRow, TenantRow};
pub use rollups::PostgresRollups;
pub use scheduling::{NewScheduledPublishRow, PostgresScheduledPublishes, ScheduledPublishRow};
pub use store::{PgTx, PostgresStore, RowUpdate};
pub use subjects::{PostgresSubjects, SubjectRow};
pub use task_health::{PostgresTaskHealth, TaskHealthRow};
pub use tax_rates::{PostgresTaxRates, TaxRateRow};
pub use translations::PostgresTranslations;
pub use vouchers::{NewVoucherRow, PostgresVouchers, VoucherRow};
pub use webhooks::{PostgresWebhooks, WebhookRow, WebhookSummaryRow};
