// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! [`PostgresStore`]: the pool, the migration, and the `EventStore` implementation.

use std::num::NonZeroU32;

use deadpool_postgres::{Manager, ManagerConfig, Object, Pool, PoolError, RecyclingMethod};
use tokio_postgres::NoTls;

use pos_ports::event_store::{AppendOutcome, EventQuery, EventStore, OutboxPosition, OutboxRecord};
use pos_ports::{PortError, PortName, Transactional, TxContext};
use pos_proto::envelope::{EventEnvelope, RawPayload};
use pos_proto::ids::{EventId, StoreId, TenantId};

/// The cloud schema, applied idempotently at start-up ([ADR-0017](../../../docs/adr/0017-migrations.md)).
const MIGRATION_0001: &str = include_str!("../migrations/0001_cloud_events.sql");

/// The rollup read model and the API-key table (P7), applied after 0001 and on the same idempotent
/// terms.
const MIGRATION_0002: &str = include_str!("../migrations/0002_cloud_rollups_apikeys.sql");

/// The super-admin credential and session schema ([ADR-0034](../../../docs/adr/0034-super-admin-auth.md)).
const MIGRATION_0003: &str = include_str!("../migrations/0003_cloud_admin.sql");

/// The four-level configuration-tree schema ([ADR-0033](../../../docs/adr/0033-config-tree.md)).
const MIGRATION_0004: &str = include_str!("../migrations/0004_cloud_config_trees.sql");

/// The subject store: where personal data lives and is masked ([ADR-0035](../../../docs/adr/0035-retention-and-pii-masking.md)).
const MIGRATION_0005: &str = include_str!("../migrations/0005_cloud_subjects.sql");

/// The webhook-endpoint table ([ADR-0032](../../../docs/adr/0032-webhooks.md)).
const MIGRATION_0006: &str = include_str!("../migrations/0006_cloud_webhooks.sql");

/// The device-proposal table ([ADR-0041](../../../docs/adr/0041-device-onboarding.md)).
const MIGRATION_0007: &str = include_str!("../migrations/0007_cloud_device_proposals.sql");

/// The translation-grid table ([ADR-0043](../../../docs/adr/0043-translation-grid.md)).
const MIGRATION_0008: &str = include_str!("../migrations/0008_cloud_translations.sql");

/// The activation-code and device-credential tables ([ADR-0050](../../../docs/adr/0050-activation-code-exchange.md)).
const MIGRATION_0009: &str = include_str!("../migrations/0009_cloud_activation.sql");

/// The cloud order-queue table (P7).
const MIGRATION_0010: &str = include_str!("../migrations/0010_cloud_order_queue.sql");

/// The cloud org registry — named Tenant/Brand/Store/Device ([ADR-0065](../../../docs/adr/0065-cloud-org-registry.md)).
const MIGRATION_0011: &str = include_str!("../migrations/0011_cloud_registry.sql");
/// The catalog authoring model (Phase 2a, ADR-0066).
const MIGRATION_0012: &str = include_str!("../migrations/0012_cloud_catalog.sql");

/// Tax classes for the catalog (Phase 2a, ADR-0066 entity 10).
const MIGRATION_0013: &str = include_str!("../migrations/0013_catalog_tax_classes.sql");

/// The operational item taxonomy — categories and sub-categories (Phase 2a, ADR-0066 entities 2/3).
const MIGRATION_0014: &str = include_str!("../migrations/0014_catalog_item_taxonomy.sql");

/// The presentation tier — display taxonomy and per-channel layout buttons (Phase 2a, ADR-0066 11/12).
const MIGRATION_0015: &str = include_str!("../migrations/0015_catalog_display_layout.sql");

/// Modifier groups (Phase 2a, ADR-0066 entities 4/5).
const MIGRATION_0016: &str = include_str!("../migrations/0016_catalog_modifier_groups.sql");

/// Menu sections (Phase 2a, ADR-0066 entity 7).
const MIGRATION_0017: &str = include_str!("../migrations/0017_catalog_menu_sections.sql");

/// Multi-admin console identities with roles ([ADR-0067](../../../docs/adr/0067-multi-admin-console-rbac.md)).
const MIGRATION_0018: &str = include_str!("../migrations/0018_cloud_admin_users.sql");

/// Sliding idle TTL + absolute cap on admin sessions ([ADR-0067](../../../docs/adr/0067-multi-admin-console-rbac.md) slice 4).
const MIGRATION_0019: &str = include_str!("../migrations/0019_admin_session_sliding.sql");

/// Fleet liveness read model — last-seen + config-version-held per store ([ADR-0068](../../../docs/adr/0068-fleet-liveness.md)).
const MIGRATION_0020: &str = include_str!("../migrations/0020_store_liveness.sql");

/// Background-task health — last-tick per loop ([ADR-0068](../../../docs/adr/0068-fleet-liveness.md) slice 4).
const MIGRATION_0021: &str = include_str!("../migrations/0021_task_health.sql");

/// The console audit trail — append-only record of who changed what ([ADR-0069](../../../docs/adr/0069-audit-trail.md)).
const MIGRATION_0022: &str = include_str!("../migrations/0022_audit_log.sql");

/// People & access, foundation — the `employees` table ([ADR-0070](../../../docs/adr/0070-people-and-access.md)).
const MIGRATION_0023: &str = include_str!("../migrations/0023_employees.sql");

/// People & access — `role_templates` + `employee_store_assignments` ([ADR-0070](../../../docs/adr/0070-people-and-access.md)).
const MIGRATION_0024: &str = include_str!("../migrations/0024_role_templates_and_assignments.sql");

/// Floor master data — `floor_areas` + `floor_tables` ([ADR-0072](../../../docs/adr/0072-floor-and-kitchen.md)).
const MIGRATION_0025: &str = include_str!("../migrations/0025_floor_areas_and_tables.sql");

/// Kitchen master data — `kitchen_stations` + `station_routing_rules` ([ADR-0072](../../../docs/adr/0072-floor-and-kitchen.md)).
const MIGRATION_0026: &str = include_str!("../migrations/0026_kitchen_stations_and_routing.sql");
/// Operational alerts ([ADR-0073](../../../docs/adr/0073-alerting.md), Track O2).
const MIGRATION_0027: &str = include_str!("../migrations/0027_alerts.sql");

/// How many pooled connections the cloud keeps to PostgreSQL.
const POOL_SIZE: usize = 16;

/// An `EventStore` and its onward outbox over PostgreSQL, behind a `deadpool` pool
/// ([ADR-0016](../../../docs/adr/0016-postgres-access.md)). Cloneable and shareable: every clone
/// draws from the same pool.
#[derive(Clone, Debug)]
pub struct PostgresStore {
    pool: Pool,
}

impl PostgresStore {
    /// Builds a store over a pool parsed from a libpq connection string (`postgres://…` or
    /// `host=… user=…`). No connection is opened until the first use.
    ///
    /// # Errors
    ///
    /// [`PortError::invalid_argument`] if the connection string does not parse, or
    /// [`PortError::internal`] if the pool cannot be built.
    pub fn connect(database_url: &str) -> Result<Self, PortError> {
        let config: tokio_postgres::Config = database_url.parse().map_err(|error| {
            PortError::invalid_argument(PortName::EventStore, "invalid database connection string")
                .with_source(error)
        })?;
        // Recycle every connection with `ROLLBACK` before it is handed out again. This is the
        // load-bearing choice for durability: a `PgTx` dropped without `commit`/`rollback` (the
        // crash the contract simulates) returns its connection to the pool with a transaction
        // still open, and deadpool's own `Clean`/`Fast` recycling does *not* end it — the next
        // caller would run inside that leaked transaction and see uncommitted rows. `ROLLBACK`
        // ends it. When there is no open transaction it is a harmless no-op (a warning, not an
        // error), so the normal begin→append→commit path pays nothing for it.
        let manager = Manager::from_config(
            config,
            NoTls,
            ManagerConfig {
                recycling_method: RecyclingMethod::Custom("ROLLBACK".to_owned()),
            },
        );
        let pool = Pool::builder(manager)
            .max_size(POOL_SIZE)
            .build()
            .map_err(|error| {
                PortError::internal(PortName::EventStore, "could not build the connection pool")
                    .with_source(error)
            })?;
        Ok(Self { pool })
    }

    /// Applies the cloud schema, idempotently — safe to run on every boot.
    ///
    /// # Errors
    ///
    /// [`PortError::unavailable`] if the database cannot be reached or a statement fails.
    #[expect(
        clippy::too_many_lines,
        reason = "one batch_execute per migration in order; a loop over a slice would hide the \
                  explicit, greppable ordering the schema history relies on"
    )]
    pub async fn migrate(&self) -> Result<(), PortError> {
        let connection = self.connection().await?;
        connection
            .batch_execute(MIGRATION_0001)
            .await
            .map_err(unavailable)?;
        connection
            .batch_execute(MIGRATION_0002)
            .await
            .map_err(unavailable)?;
        connection
            .batch_execute(MIGRATION_0003)
            .await
            .map_err(unavailable)?;
        connection
            .batch_execute(MIGRATION_0004)
            .await
            .map_err(unavailable)?;
        connection
            .batch_execute(MIGRATION_0005)
            .await
            .map_err(unavailable)?;
        connection
            .batch_execute(MIGRATION_0006)
            .await
            .map_err(unavailable)?;
        connection
            .batch_execute(MIGRATION_0007)
            .await
            .map_err(unavailable)?;
        connection
            .batch_execute(MIGRATION_0008)
            .await
            .map_err(unavailable)?;
        connection
            .batch_execute(MIGRATION_0009)
            .await
            .map_err(unavailable)?;
        connection
            .batch_execute(MIGRATION_0010)
            .await
            .map_err(unavailable)?;
        connection
            .batch_execute(MIGRATION_0011)
            .await
            .map_err(unavailable)?;
        connection
            .batch_execute(MIGRATION_0012)
            .await
            .map_err(unavailable)?;
        connection
            .batch_execute(MIGRATION_0013)
            .await
            .map_err(unavailable)?;
        connection
            .batch_execute(MIGRATION_0014)
            .await
            .map_err(unavailable)?;
        connection
            .batch_execute(MIGRATION_0015)
            .await
            .map_err(unavailable)?;
        connection
            .batch_execute(MIGRATION_0016)
            .await
            .map_err(unavailable)?;
        connection
            .batch_execute(MIGRATION_0017)
            .await
            .map_err(unavailable)?;
        connection
            .batch_execute(MIGRATION_0018)
            .await
            .map_err(unavailable)?;
        connection
            .batch_execute(MIGRATION_0019)
            .await
            .map_err(unavailable)?;
        connection
            .batch_execute(MIGRATION_0020)
            .await
            .map_err(unavailable)?;
        connection
            .batch_execute(MIGRATION_0021)
            .await
            .map_err(unavailable)?;
        connection
            .batch_execute(MIGRATION_0022)
            .await
            .map_err(unavailable)?;
        connection
            .batch_execute(MIGRATION_0023)
            .await
            .map_err(unavailable)?;
        connection
            .batch_execute(MIGRATION_0024)
            .await
            .map_err(unavailable)?;
        connection
            .batch_execute(MIGRATION_0025)
            .await
            .map_err(unavailable)?;
        connection
            .batch_execute(MIGRATION_0026)
            .await
            .map_err(unavailable)?;
        connection
            .batch_execute(MIGRATION_0027)
            .await
            .map_err(unavailable)
    }

    /// The materialised-rollup store over this pool ([ADR-0036](../../../docs/adr/0036-materialised-rollups.md)).
    ///
    /// A cheap handle sharing the same pool; `pos-cloud` implements its `RollupStore` seam over it.
    #[must_use]
    pub fn rollups(&self) -> crate::rollups::PostgresRollups {
        crate::rollups::PostgresRollups::new(self.pool.clone())
    }

    /// The API-key store over this pool ([ADR-0037](../../../docs/adr/0037-api-keys.md)).
    ///
    /// A cheap handle sharing the same pool; `pos-cloud` implements its `ApiKeyStore` seam over it.
    #[must_use]
    pub fn api_keys(&self) -> crate::apikeys::PostgresApiKeys {
        crate::apikeys::PostgresApiKeys::new(self.pool.clone())
    }

    /// The activation-code store over this pool ([ADR-0050](../../../docs/adr/0050-activation-code-exchange.md)).
    ///
    /// A cheap handle sharing the same pool; `pos-cloud` implements its `ActivationCodeStore` seam
    /// over it.
    #[must_use]
    pub fn activation_codes(&self) -> crate::activation::PostgresActivationCodes {
        crate::activation::PostgresActivationCodes::new(self.pool.clone())
    }

    /// The super-admin credential and session store over this pool ([ADR-0034](../../../docs/adr/0034-super-admin-auth.md)).
    ///
    /// A cheap handle sharing the same pool; `pos-cloud` implements its `AdminStore` seam over it.
    #[must_use]
    pub fn admin(&self) -> crate::admin::PostgresAdmin {
        crate::admin::PostgresAdmin::new(self.pool.clone())
    }

    /// The configuration-tree store over this pool ([ADR-0033](../../../docs/adr/0033-config-tree.md)).
    ///
    /// A cheap handle sharing the same pool; `pos-cloud` implements its `ConfigTreeStore` seam over it.
    #[must_use]
    pub fn config_trees(&self) -> crate::config_trees::PostgresConfigTrees {
        crate::config_trees::PostgresConfigTrees::new(self.pool.clone())
    }

    /// The subject store over this pool ([ADR-0035](../../../docs/adr/0035-retention-and-pii-masking.md)).
    ///
    /// A cheap handle sharing the same pool; `pos-cloud` implements its `SubjectStore` seam over it.
    #[must_use]
    pub fn subjects(&self) -> crate::subjects::PostgresSubjects {
        crate::subjects::PostgresSubjects::new(self.pool.clone())
    }

    /// The webhook-endpoint store over this pool ([ADR-0032](../../../docs/adr/0032-webhooks.md)).
    ///
    /// A cheap handle sharing the same pool; `pos-cloud` implements its `WebhookEndpointStore` seam
    /// over it.
    #[must_use]
    pub fn webhooks(&self) -> crate::webhooks::PostgresWebhooks {
        crate::webhooks::PostgresWebhooks::new(self.pool.clone())
    }

    /// The order-queue store over this pool (P7).
    ///
    /// A cheap handle sharing the same pool; `pos-cloud` implements its `OrderQueueStore` seam over
    /// it.
    #[must_use]
    pub fn order_queue(&self) -> crate::order_queue::PostgresOrderQueue {
        crate::order_queue::PostgresOrderQueue::new(self.pool.clone())
    }

    /// The store→tenant directory over this pool (P7).
    ///
    /// A cheap handle sharing the same pool; `pos-cloud` implements its `StoreDirectory` seam over it.
    #[must_use]
    pub fn store_directory(&self) -> crate::order_queue::PostgresStoreDirectory {
        crate::order_queue::PostgresStoreDirectory::new(self.pool.clone())
    }

    /// The reconciliation query over this pool ([ADR-0040](../../../docs/adr/0040-reconciliation.md)).
    ///
    /// A cheap handle sharing the same pool; `pos-cloud` implements its `ReconcileStore` seam over it.
    #[must_use]
    pub fn reconcile(&self) -> crate::reconcile::PostgresReconcile {
        crate::reconcile::PostgresReconcile::new(self.pool.clone())
    }

    /// The device-proposal store over this pool ([ADR-0041](../../../docs/adr/0041-device-onboarding.md)).
    ///
    /// A cheap handle sharing the same pool; `pos-cloud` implements its `DeviceProposalStore` seam
    /// over it.
    #[must_use]
    pub fn device_proposals(&self) -> crate::devices::PostgresDeviceProposals {
        crate::devices::PostgresDeviceProposals::new(self.pool.clone())
    }

    /// The translation-grid store over this pool ([ADR-0043](../../../docs/adr/0043-translation-grid.md)).
    ///
    /// A cheap handle sharing the same pool; `pos-cloud` implements its `TranslationStore` seam over
    /// it.
    #[must_use]
    pub fn translations(&self) -> crate::translations::PostgresTranslations {
        crate::translations::PostgresTranslations::new(self.pool.clone())
    }

    /// The org-registry store over this pool ([ADR-0065](../../../docs/adr/0065-cloud-org-registry.md)).
    ///
    /// A cheap handle sharing the same pool; `pos-cloud` implements its `RegistryStore` seam over it.
    #[must_use]
    pub fn registry(&self) -> crate::registry::PostgresRegistry {
        crate::registry::PostgresRegistry::new(self.pool.clone())
    }

    /// The fleet read model over this pool ([ADR-0068](../../../docs/adr/0068-fleet-liveness.md)).
    ///
    /// A cheap handle sharing the same pool; `pos-cloud` implements its `FleetStore` seam over it. A
    /// read-only join across `stores`, `store_liveness`, `config_trees`, and `order_queue`.
    #[must_use]
    pub fn fleet(&self) -> crate::fleet::PostgresFleet {
        crate::fleet::PostgresFleet::new(self.pool.clone())
    }

    /// The background-task health store over this pool ([ADR-0068](../../../docs/adr/0068-fleet-liveness.md) slice 4).
    ///
    /// A cheap handle sharing the same pool; `pos-cloud` implements its `TaskHealthStore` seam over
    /// it. Each background loop records its tick here; the `/admin` health route reads it.
    #[must_use]
    pub fn task_health(&self) -> crate::task_health::PostgresTaskHealth {
        crate::task_health::PostgresTaskHealth::new(self.pool.clone())
    }

    /// The console audit trail over this pool ([ADR-0069](../../../docs/adr/0069-audit-trail.md)).
    ///
    /// A cheap handle sharing the same pool; `pos-cloud` implements its `AuditStore` seam over it. An
    /// append-only record of who changed what across the `/admin` write routes.
    #[must_use]
    pub fn audit(&self) -> crate::audit::PostgresAudit {
        crate::audit::PostgresAudit::new(self.pool.clone())
    }

    /// The operational-alert store over this pool ([ADR-0073](../../../docs/adr/0073-alerting.md), Track O2).
    ///
    /// A cheap handle sharing the same pool; `pos-cloud` implements its `AlertStore` seam over it. The
    /// alert evaluator opens/refreshes/resolves alerts here; the `/admin` alerts route reads them.
    #[must_use]
    pub fn alerts(&self) -> crate::alerts::PostgresAlerts {
        crate::alerts::PostgresAlerts::new(self.pool.clone())
    }

    /// The employee store over this pool (Track M1, [ADR-0070](../../../docs/adr/0070-people-and-access.md)).
    ///
    /// A cheap handle sharing the same pool; `pos-cloud` implements its `EmployeeStore` seam over it.
    /// Tenant-scoped, RLS-isolated; the PIN is held only as its Argon2id hash and never read out.
    #[must_use]
    pub fn people(&self) -> crate::people::PostgresPeople {
        crate::people::PostgresPeople::new(self.pool.clone())
    }

    /// The floor master-data store over this pool (Track M2, [ADR-0072](../../../docs/adr/0072-floor-and-kitchen.md)).
    ///
    /// A cheap handle sharing the same pool; `pos-cloud` implements its `AreaStore`/`TableStore` seams
    /// over it. Tenant-scoped and per-store; areas and tables are archived, never deleted.
    #[must_use]
    pub fn floor(&self) -> crate::floor::PostgresFloor {
        crate::floor::PostgresFloor::new(self.pool.clone())
    }

    /// The catalog authoring store over this pool (Phase 2a, [ADR-0066](../../../docs/adr/0066-cloud-catalog.md)).
    ///
    /// A cheap handle sharing the same pool; `pos-cloud` implements its `CatalogStore` seam over it and
    /// compiles the authored model into a `MenuBook`.
    #[must_use]
    pub fn catalog(&self) -> crate::catalog::PostgresCatalog {
        crate::catalog::PostgresCatalog::new(self.pool.clone())
    }

    /// Every `(tenant, store)` that has ever recorded an event — the fleet the rollup projector keeps
    /// current ([ADR-0036](../../../docs/adr/0036-materialised-rollups.md)).
    ///
    /// Read as the trusted role, so it spans every tenant (RLS bypassed) — the projector maintains
    /// the whole fleet's rollups, not one tenant's. A row whose ids are not ULIDs (impossible for
    /// rows this adapter wrote) is skipped rather than failing the whole listing.
    ///
    /// # Errors
    ///
    /// [`PortError::unavailable`] if the database cannot be reached.
    pub async fn list_active_stores(&self) -> Result<Vec<(TenantId, StoreId)>, PortError> {
        let connection = self.connection().await?;
        let rows = connection
            .query("SELECT DISTINCT tenant_id, store_id FROM events", &[])
            .await
            .map_err(unavailable)?;
        Ok(rows
            .iter()
            .filter_map(|row| {
                let tenant: String = row.get(0);
                let store: String = row.get(1);
                Some((tenant.parse().ok()?, store.parse().ok()?))
            })
            .collect())
    }

    /// Creates the monthly partition covering `business_date` (an `YYYY-MM-DD` string), ahead of
    /// need. Idempotent. The cloud scheduler calls this before a month is written to (ADR-0022).
    ///
    /// # Errors
    ///
    /// [`PortError::unavailable`] if the database cannot be reached, or [`PortError::invalid_argument`]
    /// if the date does not parse.
    pub async fn ensure_partition(&self, business_date: &str) -> Result<(), PortError> {
        let connection = self.connection().await?;
        connection
            .execute(
                "SELECT create_events_partition(to_date($1, 'YYYY-MM-DD'))",
                &[&business_date],
            )
            .await
            .map_err(unavailable)?;
        Ok(())
    }

    async fn connection(&self) -> Result<Object, PortError> {
        self.pool.get().await.map_err(pool_unavailable)
    }
}

/// A write transaction over a pooled connection. `begin` issues `BEGIN`; [`TxContext::commit`] and
/// [`TxContext::rollback`] issue `COMMIT`/`ROLLBACK` and return the connection to the pool. Owned,
/// borrowing nothing, so it is `Send` and can be held across a spawn (ADR-0026 §2).
#[derive(Debug)]
pub struct PgTx {
    connection: Object,
}

impl TxContext for PgTx {
    async fn commit(self) -> Result<(), PortError> {
        self.connection
            .batch_execute("COMMIT")
            .await
            .map_err(unavailable)
    }

    async fn rollback(self) -> Result<(), PortError> {
        self.connection
            .batch_execute("ROLLBACK")
            .await
            .map_err(unavailable)
    }
}

impl Transactional for PostgresStore {
    type Tx = PgTx;

    async fn begin(&self) -> Result<Self::Tx, PortError> {
        let connection = self.connection().await?;
        connection
            .batch_execute("BEGIN")
            .await
            .map_err(unavailable)?;
        Ok(PgTx { connection })
    }
}

impl EventStore for PostgresStore {
    async fn append(
        &self,
        tx: &mut Self::Tx,
        events: &[EventEnvelope<RawPayload>],
    ) -> Result<AppendOutcome, PortError> {
        let Some(first) = events.first() else {
            return Ok(AppendOutcome::default());
        };
        // A batch is one store's, so the outbox stays per-store and a mixed batch is a caller bug.
        if events.iter().any(|event| event.store_id != first.store_id) {
            return Err(PortError::invalid_argument(
                PortName::EventStore,
                "a batch must belong to a single store",
            ));
        }

        let mut outcome = AppendOutcome::default();
        for event in events {
            // Serialised to text and stored in a `json` column, which keeps the exact bytes. The
            // contract requires a replayed event to read back identical to the first writer's, so
            // this must not go through anything that normalises — see the migration's note on
            // `json` vs `jsonb`. The parameter is cast `$5::text::json` rather than `$5::json`
            // because the latter makes PostgreSQL infer the parameter itself as `json`, which the
            // text we bind cannot satisfy; the `::text` step pins the inference to `text` first.
            let envelope = serde_json::to_string(event).map_err(encode)?;
            let business_date = event.business_date.to_string();
            let event_id = event.event_id.to_string();
            let tenant_id = event.tenant_id.to_string();
            let store_id = event.store_id.to_string();

            // Idempotent by (business_date, event_id) — which is event_id in practice, since a replay
            // carries the same business_date. The stored copy wins; the incoming one is discarded.
            let inserted = tx
                .connection
                .execute(
                    "INSERT INTO events (business_date, event_id, tenant_id, store_id, envelope) \
                     VALUES (to_date($1, 'YYYY-MM-DD'), $2, $3, $4, $5::text::json) \
                     ON CONFLICT (business_date, event_id) DO NOTHING",
                    &[&business_date, &event_id, &tenant_id, &store_id, &envelope],
                )
                .await
                .map_err(unavailable)?;

            if inserted == 1 {
                outcome.appended = outcome.appended.saturating_add(1);
                tx.connection
                    .execute(
                        "INSERT INTO event_outbox (store_id, envelope) VALUES ($1, $2::text::json)",
                        &[&store_id, &envelope],
                    )
                    .await
                    .map_err(unavailable)?;
            } else {
                outcome.duplicates = outcome.duplicates.saturating_add(1);
            }
        }
        Ok(outcome)
    }

    async fn read(&self, query: &EventQuery) -> Result<Vec<EventEnvelope<RawPayload>>, PortError> {
        let connection = self.connection().await?;
        let store_id = query.store_id.to_string();
        let limit = i64::from(query.limit.get());
        // ULID strings sort lexicographically in event-time order, so ordering and the `after`
        // cursor are plain text comparisons.
        let rows =
            match query.after {
                Some(after) => connection
                    .query(
                        "SELECT envelope::text FROM events WHERE store_id = $1 AND event_id > $2 \
                         ORDER BY event_id ASC LIMIT $3",
                        &[&store_id, &after.to_string(), &limit],
                    )
                    .await,
                None => {
                    connection
                        .query(
                            "SELECT envelope::text FROM events WHERE store_id = $1 \
                         ORDER BY event_id ASC LIMIT $2",
                            &[&store_id, &limit],
                        )
                        .await
                }
            }
            .map_err(unavailable)?;

        let mut events = Vec::with_capacity(rows.len());
        for row in rows {
            let envelope: String = row.get(0);
            events.push(serde_json::from_str(&envelope).map_err(encode)?);
        }
        Ok(events)
    }

    async fn contains(&self, store_id: StoreId, event_id: EventId) -> Result<bool, PortError> {
        let connection = self.connection().await?;
        let row = connection
            .query_one(
                "SELECT EXISTS(SELECT 1 FROM events WHERE store_id = $1 AND event_id = $2)",
                &[&store_id.to_string(), &event_id.to_string()],
            )
            .await
            .map_err(unavailable)?;
        Ok(row.get(0))
    }

    async fn outbox_batch(
        &self,
        store_id: StoreId,
        after: OutboxPosition,
        limit: NonZeroU32,
    ) -> Result<Vec<OutboxRecord>, PortError> {
        let connection = self.connection().await?;
        let after = i64::try_from(after.get()).unwrap_or(i64::MAX);
        let limit = i64::from(limit.get());
        let rows = connection
            .query(
                "SELECT position, envelope::text FROM event_outbox \
                 WHERE store_id = $1 AND position > $2 ORDER BY position ASC LIMIT $3",
                &[&store_id.to_string(), &after, &limit],
            )
            .await
            .map_err(unavailable)?;

        let mut records = Vec::with_capacity(rows.len());
        for row in rows {
            let position: i64 = row.get(0);
            let envelope: String = row.get(1);
            records.push(OutboxRecord {
                position: OutboxPosition::new(u64::try_from(position).unwrap_or(0)),
                envelope: serde_json::from_str(&envelope).map_err(encode)?,
            });
        }
        Ok(records)
    }

    async fn acknowledge_outbox(
        &self,
        store_id: StoreId,
        through: OutboxPosition,
    ) -> Result<u64, PortError> {
        let connection = self.connection().await?;
        let through = i64::try_from(through.get()).unwrap_or(i64::MAX);
        connection
            .execute(
                "DELETE FROM event_outbox WHERE store_id = $1 AND position <= $2",
                &[&store_id.to_string(), &through],
            )
            .await
            .map_err(unavailable)
    }

    async fn outbox_depth(&self, store_id: StoreId) -> Result<u64, PortError> {
        let connection = self.connection().await?;
        let row = connection
            .query_one(
                "SELECT count(*) FROM event_outbox WHERE store_id = $1",
                &[&store_id.to_string()],
            )
            .await
            .map_err(unavailable)?;
        let count: i64 = row.get(0);
        Ok(u64::try_from(count).unwrap_or(0))
    }
}

/// Maps a database error to the port's unavailable status.
pub(crate) fn unavailable(error: tokio_postgres::Error) -> PortError {
    PortError::unavailable(PortName::EventStore, "the cloud database failed").with_source(error)
}

/// Maps a pool checkout failure (no connection available) to the port's unavailable status.
pub(crate) fn pool_unavailable(error: PoolError) -> PortError {
    PortError::unavailable(PortName::EventStore, "the cloud database is unavailable")
        .with_source(error)
}

/// Maps an envelope (de)serialisation failure to the port's internal status.
fn encode(error: serde_json::Error) -> PortError {
    PortError::internal(
        PortName::EventStore,
        "could not (de)serialise an event envelope",
    )
    .with_source(error)
}
