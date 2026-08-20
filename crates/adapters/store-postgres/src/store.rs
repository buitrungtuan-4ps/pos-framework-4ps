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
    pub async fn migrate(&self) -> Result<(), PortError> {
        let connection = self.connection().await?;
        connection
            .batch_execute(MIGRATION_0001)
            .await
            .map_err(unavailable)?;
        connection
            .batch_execute(MIGRATION_0002)
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
