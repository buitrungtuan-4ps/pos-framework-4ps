// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! Scheduled, effective-dated config publishes (Track M3, [ADR-0077](../../../docs/adr/0077-campaigns-and-scheduling.md)).
//!
//! Every other publish is immediate: a handler compiles a node and versions it *now*. A Tết menu, or a
//! midnight price change, needs it to switch on *then* — without a human awake at 00:00. This module is
//! that mechanism: a **snapshot** of a node value, its target `(store, node key)`, and an `effective_at`
//! sit in a table until a background **activator** applies them at their time, through the same config
//! tree the immediate publishes use.
//!
//! Snapshot-at-schedule, not recompute-at-fire: the value stored is what was authored and reviewed when
//! the publish was scheduled, so later edits never leak into a publish nobody looked at again. The
//! mechanism is node-agnostic — the activator writes whatever `node_key`/`node_value` a row carries onto
//! the Store layer — so menu, tax, and campaign publishes can all be future-dated; this track wires the
//! campaign schedule route, the rest reuse the same store and activator.

use core::future::Future;
use core::time::Duration;

use pos_proto::ids::{ConfigVersionId, StoreId, TenantId};
use pos_proto::ulid::Ulid;

use pos_proto::ClockSource;

use crate::config_tree::{
    CapabilityValidator, ConfigError, ConfigLevel, ConfigTree, ConfigTreeStore,
};
use crate::health::{TaskHealthStore, tick_detail};
use crate::version::UpdateOutcome;

/// The canonical name of the scheduled-publish activator loop, for task-health reporting.
pub const SCHEDULED_PUBLISH_ACTIVATOR: &str = "scheduled_publish_activator";

/// A scheduled publish's lifecycle status.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ScheduledPublishStatus {
    /// Waiting for its effective time; the activator will apply it.
    Pending,
    /// Applied — its node was published as a config version.
    Applied,
    /// Cancelled by an operator before it fired; never applied.
    Cancelled,
}

impl ScheduledPublishStatus {
    /// The stored token.
    #[must_use]
    pub const fn as_wire(self) -> &'static str {
        match self {
            Self::Pending => "PENDING",
            Self::Applied => "APPLIED",
            Self::Cancelled => "CANCELLED",
        }
    }

    /// Parses a stored token, defaulting an unknown one to [`Pending`](ScheduledPublishStatus::Pending).
    #[must_use]
    pub fn from_wire(token: &str) -> Self {
        match token {
            "APPLIED" => Self::Applied,
            "CANCELLED" => Self::Cancelled,
            _ => Self::Pending,
        }
    }
}

/// A publish to schedule: the snapshot and when to apply it.
#[derive(Debug, Clone)]
pub struct NewScheduledPublish {
    /// The row's id (a ULID string), server-minted.
    pub id: String,
    /// The tenant the store belongs to.
    pub tenant_id: TenantId,
    /// The store to publish to.
    pub store_id: StoreId,
    /// The Store-layer key to write (`campaigns`, `menu`, `tax`, …).
    pub node_key: String,
    /// The snapshotted node value, as it was compiled at schedule time.
    pub node_value: serde_json::Value,
    /// When to apply it, Unix milliseconds.
    pub effective_at_ms: i64,
    /// The admin who scheduled it (id string), for the audit trail.
    pub created_by: String,
}

/// A stored scheduled publish.
#[derive(Debug, Clone)]
pub struct ScheduledPublish {
    /// The row's id.
    pub id: String,
    /// The tenant.
    pub tenant_id: TenantId,
    /// The store.
    pub store_id: StoreId,
    /// The Store-layer key.
    pub node_key: String,
    /// The snapshotted node value.
    pub node_value: serde_json::Value,
    /// When it applies, Unix milliseconds.
    pub effective_at_ms: i64,
    /// Its status.
    pub status: ScheduledPublishStatus,
    /// When it was scheduled, Unix milliseconds.
    pub created_at_ms: i64,
    /// The config version it published as, once applied.
    pub applied_version_id: Option<String>,
}

/// Persists and reads scheduled publishes.
///
/// `schedule` inserts a pending row; `due` reads every pending row whose time has come (across all
/// tenants — the activator is fleet-wide, read as the trusted owner); `list_for_store` reads a store's
/// pending publishes for the console; `cancel` withdraws one; `mark_applied` records that a row
/// published as a given version. Writes and per-store reads are tenant-scoped.
pub trait ScheduledPublishStore {
    /// Schedules a publish.
    fn schedule(
        &self,
        publish: &NewScheduledPublish,
    ) -> impl Future<Output = Result<(), ScheduledPublishError>> + Send;

    /// Every pending publish whose `effective_at` is at or before `now_ms`, across all tenants.
    fn due(
        &self,
        now_ms: i64,
    ) -> impl Future<Output = Result<Vec<ScheduledPublish>, ScheduledPublishError>> + Send;

    /// A store's pending publishes, soonest first, for the console.
    fn list_for_store(
        &self,
        tenant_id: TenantId,
        store_id: StoreId,
    ) -> impl Future<Output = Result<Vec<ScheduledPublish>, ScheduledPublishError>> + Send;

    /// Cancels a pending publish. Returns whether a pending row with that id existed.
    fn cancel(
        &self,
        tenant_id: TenantId,
        id: &str,
    ) -> impl Future<Output = Result<bool, ScheduledPublishError>> + Send;

    /// Marks a publish applied, recording the config version it produced. Only a still-pending row is
    /// moved, so a row cannot be applied twice.
    fn mark_applied(
        &self,
        id: &str,
        version_id: &str,
    ) -> impl Future<Output = Result<(), ScheduledPublishError>> + Send;
}

/// A failure of the scheduled-publish store itself.
#[derive(Debug, thiserror::Error)]
#[error("the scheduled-publish store failed: {0}")]
pub struct ScheduledPublishError(String);

impl ScheduledPublishError {
    /// Wraps a message (for the server's log).
    #[must_use]
    pub fn new(message: impl Into<String>) -> Self {
        Self(message.into())
    }
}

/// A fresh config version id from `now_ms` plus OS entropy, or `None` if entropy is unavailable — the
/// same shape the immediate-publish handlers use.
fn mint_version_id(now_ms: i64) -> Option<ConfigVersionId> {
    let mut bytes = [0_u8; 16];
    getrandom::fill(&mut bytes).ok()?;
    let ms = u64::try_from(now_ms.max(0)).unwrap_or(0);
    Some(ConfigVersionId::new(Ulid::from_parts(
        ms,
        u128::from_le_bytes(bytes),
    )))
}

/// Applies one scheduled publish: sets its snapshotted node on the store's Store layer and versions it
/// through the config tree — the same load→merge→publish→save shape the immediate publishes use.
/// Returns the new config version id.
async fn apply_one<Cfg>(
    config_trees: &Cfg,
    publish: &ScheduledPublish,
    now_ms: i64,
) -> Result<ConfigVersionId, String>
where
    Cfg: ConfigTreeStore,
{
    let loaded = config_trees
        .load(publish.tenant_id, publish.store_id)
        .await
        .map_err(|error| error.to_string())?;
    let (state_before, row_version) = match loaded {
        Some(versioned) => (Some(versioned.record), Some(versioned.etag)),
        None => (None, None),
    };
    let mut store_layer = state_before.as_ref().map_or_else(
        || serde_json::Value::Object(serde_json::Map::new()),
        |existing| existing.layers[2].clone(),
    );
    if !store_layer.is_object() {
        store_layer = serde_json::Value::Object(serde_json::Map::new());
    }
    if let serde_json::Value::Object(map) = &mut store_layer {
        map.insert(publish.node_key.clone(), publish.node_value.clone());
    }
    let mut tree = match state_before {
        Some(existing) => ConfigTree::from_state(publish.store_id, CapabilityValidator, existing),
        None => ConfigTree::new(publish.store_id, CapabilityValidator),
    };
    let version_id = mint_version_id(now_ms).ok_or_else(|| "OS entropy unavailable".to_owned())?;
    match tree.publish(ConfigLevel::Store, store_layer, version_id) {
        Ok(id) => {
            // The activator has no operator and no `If-Match`, but it still composes on what it
            // read, so it takes the same precondition every console write does
            // ([ADR-0095](../../../docs/adr/0095-conditional-writes-for-collections.md)). Losing the
            // race is not an error: the row stays pending and the next tick re-reads and retries,
            // which is exactly what `pass` already does for every other failure.
            match config_trees
                .save(
                    publish.tenant_id,
                    publish.store_id,
                    &tree.state(),
                    row_version.as_ref(),
                )
                .await
                .map_err(|error| error.to_string())?
            {
                UpdateOutcome::Updated(_) => Ok(id),
                UpdateOutcome::VersionMismatch | UpdateOutcome::NotFound => Err(
                    "the store's configuration changed while this scheduled publish was being \
                     applied; it stays pending and the next pass will retry it"
                        .to_owned(),
                ),
            }
        }
        Err(ConfigError::Invalid(violations)) => {
            Err(format!("the scheduled node is invalid: {violations:?}"))
        }
    }
}

/// One activator pass: applies every publish whose time has come. A row that fails to apply is logged
/// and left pending to retry on the next tick, so one bad snapshot never blocks the others. Returns
/// how many were applied.
async fn pass<S, Cfg>(scheduled: &S, config_trees: &Cfg, now_ms: i64) -> Result<usize, String>
where
    S: ScheduledPublishStore,
    Cfg: ConfigTreeStore,
{
    let due = scheduled
        .due(now_ms)
        .await
        .map_err(|error| error.to_string())?;
    let mut applied = 0_usize;
    for publish in due {
        match apply_one(config_trees, &publish, now_ms).await {
            Ok(version_id) => {
                if let Err(error) = scheduled
                    .mark_applied(&publish.id, &version_id.to_string())
                    .await
                {
                    // The node published but the row did not flip: log it. Next tick re-applies the
                    // same snapshot (idempotent — it just versions the identical node again) and
                    // retries the mark, rather than losing the publish.
                    tracing::error!(%error, id = %publish.id, "scheduled publish applied but could not be marked");
                } else {
                    applied += 1;
                }
            }
            Err(error) => {
                tracing::error!(%error, id = %publish.id, node = %publish.node_key, "a scheduled publish failed to apply; left pending to retry");
            }
        }
    }
    Ok(applied)
}

/// Runs the scheduled-publish activator until `shutdown` resolves. Each tick applies every publish
/// whose effective time has arrived and records its own health — the same supervised-loop shape as the
/// retention and alert loops.
pub async fn run<S, Cfg, Th, C>(
    scheduled: S,
    config_trees: Cfg,
    task_health: Th,
    clock: C,
    interval: Duration,
    shutdown: impl Future<Output = ()>,
) where
    S: ScheduledPublishStore + Sync,
    Cfg: ConfigTreeStore + Sync,
    Th: TaskHealthStore + Sync,
    C: ClockSource,
{
    tokio::pin!(shutdown);
    loop {
        let now = clock.now();
        let detail = match pass(&scheduled, &config_trees, now.as_milliseconds_since_epoch()).await
        {
            Ok(applied) => {
                if applied > 0 {
                    tracing::info!(applied, "scheduled-publish activator applied due publishes");
                }
                tick_detail(
                    true,
                    interval.as_secs(),
                    serde_json::json!({ "applied": applied }),
                )
            }
            Err(error) => {
                tracing::error!(%error, "scheduled-publish activator pass failed; will retry");
                tick_detail(false, interval.as_secs(), serde_json::json!({}))
            }
        };
        if let Err(error) = task_health
            .record_tick(SCHEDULED_PUBLISH_ACTIVATOR, clock.now(), &detail)
            .await
        {
            tracing::warn!(%error, "recording scheduled-publish-activator task health failed");
        }
        tokio::select! {
            biased;
            () = &mut shutdown => {
                tracing::info!("scheduled-publish activator shutting down");
                return;
            }
            () = tokio::time::sleep(interval) => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use pos_proto::ids::{StoreId, TenantId};
    use pos_proto::ulid::Ulid;

    use super::{
        NewScheduledPublish, ScheduledPublish, ScheduledPublishError, ScheduledPublishStatus,
        ScheduledPublishStore,
    };
    use crate::config_tree::ConfigTreeStore;

    #[derive(Default)]
    struct FakeScheduled {
        rows: Mutex<Vec<ScheduledPublish>>,
    }

    impl ScheduledPublishStore for FakeScheduled {
        async fn schedule(
            &self,
            publish: &NewScheduledPublish,
        ) -> Result<(), ScheduledPublishError> {
            self.rows.lock().expect("lock").push(ScheduledPublish {
                id: publish.id.clone(),
                tenant_id: publish.tenant_id,
                store_id: publish.store_id,
                node_key: publish.node_key.clone(),
                node_value: publish.node_value.clone(),
                effective_at_ms: publish.effective_at_ms,
                status: ScheduledPublishStatus::Pending,
                created_at_ms: 0,
                applied_version_id: None,
            });
            Ok(())
        }

        async fn due(&self, now_ms: i64) -> Result<Vec<ScheduledPublish>, ScheduledPublishError> {
            Ok(self
                .rows
                .lock()
                .expect("lock")
                .iter()
                .filter(|row| {
                    row.status == ScheduledPublishStatus::Pending && row.effective_at_ms <= now_ms
                })
                .cloned()
                .collect())
        }

        async fn list_for_store(
            &self,
            tenant_id: TenantId,
            store_id: StoreId,
        ) -> Result<Vec<ScheduledPublish>, ScheduledPublishError> {
            Ok(self
                .rows
                .lock()
                .expect("lock")
                .iter()
                .filter(|row| {
                    row.tenant_id == tenant_id
                        && row.store_id == store_id
                        && row.status == ScheduledPublishStatus::Pending
                })
                .cloned()
                .collect())
        }

        async fn cancel(
            &self,
            tenant_id: TenantId,
            id: &str,
        ) -> Result<bool, ScheduledPublishError> {
            let mut rows = self.rows.lock().expect("lock");
            for row in rows.iter_mut() {
                if row.tenant_id == tenant_id
                    && row.id == id
                    && row.status == ScheduledPublishStatus::Pending
                {
                    row.status = ScheduledPublishStatus::Cancelled;
                    return Ok(true);
                }
            }
            Ok(false)
        }

        async fn mark_applied(
            &self,
            id: &str,
            version_id: &str,
        ) -> Result<(), ScheduledPublishError> {
            let mut rows = self.rows.lock().expect("lock");
            for row in rows.iter_mut() {
                if row.id == id && row.status == ScheduledPublishStatus::Pending {
                    row.status = ScheduledPublishStatus::Applied;
                    row.applied_version_id = Some(version_id.to_owned());
                }
            }
            Ok(())
        }
    }

    fn tenant() -> TenantId {
        TenantId::new(Ulid::from_u128(1))
    }

    fn store() -> StoreId {
        StoreId::new(Ulid::from_u128(2))
    }

    fn new_publish(id: &str, effective_at_ms: i64) -> NewScheduledPublish {
        NewScheduledPublish {
            id: id.to_owned(),
            tenant_id: tenant(),
            store_id: store(),
            node_key: "campaigns".to_owned(),
            node_value: serde_json::json!({ "campaigns": [] }),
            effective_at_ms,
            created_by: "admin-1".to_owned(),
        }
    }

    #[tokio::test]
    async fn due_returns_only_ripe_pending_rows_and_cancel_withdraws() {
        let store_seam = FakeScheduled::default();
        store_seam
            .schedule(&new_publish("past", 1_000))
            .await
            .expect("schedule past");
        store_seam
            .schedule(&new_publish("future", 9_999_999))
            .await
            .expect("schedule future");

        // Only the ripe one is due at t=5000.
        let due = store_seam.due(5_000).await.expect("due");
        assert_eq!(due.len(), 1);
        assert_eq!(due.first().expect("row").id, "past");

        // Applying it removes it from due and records the version.
        store_seam
            .mark_applied("past", "ver-1")
            .await
            .expect("apply");
        assert!(store_seam.due(5_000).await.expect("due again").is_empty());

        // The future one lists for the store and can be cancelled.
        assert_eq!(
            store_seam
                .list_for_store(tenant(), store())
                .await
                .expect("list")
                .len(),
            1
        );
        assert!(store_seam.cancel(tenant(), "future").await.expect("cancel"));
        assert!(
            store_seam
                .list_for_store(tenant(), store())
                .await
                .expect("list after cancel")
                .is_empty()
        );
        // Cancelling an unknown id is a no-op, not an error.
        assert!(
            !store_seam
                .cancel(tenant(), "nope")
                .await
                .expect("cancel nope")
        );
    }

    /// An in-memory `ConfigTreeStore` for the activator's pass test.
    #[derive(Default)]
    struct FakeConfigTrees {
        states: Mutex<Vec<(TenantId, StoreId, crate::config_tree::ConfigTreeState)>>,
    }

    impl ConfigTreeStore for FakeConfigTrees {
        async fn load(
            &self,
            tenant: TenantId,
            store: StoreId,
        ) -> Result<
            Option<crate::version::Versioned<crate::config_tree::ConfigTreeState>>,
            crate::config_tree::ConfigStoreError,
        > {
            Ok(self
                .states
                .lock()
                .expect("lock")
                .iter()
                .find(|(t, s, _)| *t == tenant && *s == store)
                .map(|(_, _, state)| {
                    crate::version::Versioned::new(state.clone(), crate::version::Version::new("1"))
                }))
        }

        async fn save(
            &self,
            tenant: TenantId,
            store: StoreId,
            state: &crate::config_tree::ConfigTreeState,
            _expected: Option<&crate::version::Version>,
        ) -> Result<crate::version::UpdateOutcome, crate::config_tree::ConfigStoreError> {
            let mut states = self.states.lock().expect("lock");
            states.retain(|(t, s, _)| !(*t == tenant && *s == store));
            states.push((tenant, store, state.clone()));
            Ok(crate::version::UpdateOutcome::Updated(
                crate::version::Version::new("1"),
            ))
        }

        async fn record_store_seen(
            &self,
            _tenant: TenantId,
            _store: StoreId,
            _held_version: Option<pos_proto::ids::ConfigVersionId>,
            _seen_at: pos_proto::time::Timestamp,
        ) -> Result<(), crate::config_tree::ConfigStoreError> {
            Ok(())
        }

        async fn record_store_heartbeat(
            &self,
            _tenant: TenantId,
            _store: StoreId,
            _seen_at: pos_proto::time::Timestamp,
            _outbox_depth: Option<u64>,
            _lease_generation: Option<u64>,
        ) -> Result<(), crate::config_tree::ConfigStoreError> {
            Ok(())
        }
    }

    #[tokio::test]
    async fn a_pass_applies_a_due_publish_to_the_config_tree_and_marks_it_applied() {
        let scheduled = FakeScheduled::default();
        let config_trees = FakeConfigTrees::default();
        scheduled
            .schedule(&new_publish("due-1", 1_000))
            .await
            .expect("schedule");

        // Run one pass at a time past the effective instant.
        let applied = super::pass(&scheduled, &config_trees, 5_000)
            .await
            .expect("pass");
        assert_eq!(applied, 1);

        // The config tree now carries the `campaigns` key on the Store layer (index 2).
        let state = config_trees
            .load(tenant(), store())
            .await
            .expect("load")
            .expect("a tree was saved");
        assert!(
            state.record.layers[2]
                .as_object()
                .is_some_and(|map| map.contains_key("campaigns")),
            "the scheduled campaigns node was published onto the Store layer"
        );

        // The row is now applied and out of the due set, so a second pass is a no-op.
        assert!(scheduled.due(5_000).await.expect("due").is_empty());
        assert_eq!(
            super::pass(&scheduled, &config_trees, 5_000)
                .await
                .expect("second pass"),
            0
        );
    }
}
