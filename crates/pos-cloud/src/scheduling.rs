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
use crate::releases::{PairTally, ReleaseStatus, ReleaseStore, status_from_tally};
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
    /// The release this pair belongs to, or `None` for a standalone schedule.
    ///
    /// `None` is ADR-0077's original shape and stays exactly as it was: the per-store campaign
    /// schedule is not a release and does not become one
    /// ([ADR-0125](../../../docs/adr/0125-a-release-is-one-decision-many-writes.md)).
    pub release_id: Option<String>,
    /// The IANA zone this row's instant was resolved against, or `None`.
    ///
    /// Written beside the instant rather than derived from the store later, because it describes a
    /// decision an operator approved: "04:00 at Ginza" and the same moment read elsewhere are one
    /// number and two sentences, and
    /// [ADR-0125](../../../docs/adr/0125-a-release-is-one-decision-many-writes.md) §26 rejected
    /// fire-time conversion so the operator could be shown the first. A store whose published zone
    /// changes afterwards must not silently re-describe a schedule that was already signed off.
    ///
    /// `None` for a release timed as a plain UTC instant, which needs no zone, for ADR-0077's
    /// standalone schedules, which are not releases, and for any row written before the column
    /// existed. All three mean the same thing to a reader: this row cannot name its clock.
    pub resolved_timezone: Option<String>,
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
    /// The release this pair belongs to, or `None` for a standalone schedule.
    pub release_id: Option<String>,
    /// Why it last failed to apply, or `None`.
    ///
    /// Recorded on the row rather than only logged, because a release's `partial` is *derived* from
    /// its pairs (ADR-0125 §4) and a failure nobody can read is a release that reports itself as
    /// still trying. A row that fails stays `Pending` and is retried; the text is the most recent
    /// reason, not a history.
    pub failure: Option<String>,
    /// The IANA zone this row's instant was resolved against, or `None`.
    ///
    /// The read side of [`NewScheduledPublish::resolved_timezone`], and what lets the console print
    /// "04:00 Asia/Tokyo" where it could previously only print an instant in whatever clock the
    /// reader's browser was in.
    pub resolved_timezone: Option<String>,
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

    /// Records why a publish last failed. The row stays pending and is retried on the next pass.
    ///
    /// Separate from [`mark_applied`](ScheduledPublishStore::mark_applied) because a failure is not
    /// a terminal state here: the activator retries, and what an operator needs meanwhile is the
    /// reason, on the row, where the release's report reads it.
    fn mark_failed(
        &self,
        id: &str,
        failure: &str,
    ) -> impl Future<Output = Result<(), ScheduledPublishError>> + Send;

    /// Every pair of one release, for the node × store report.
    fn list_for_release(
        &self,
        tenant_id: TenantId,
        release_id: &str,
    ) -> impl Future<Output = Result<Vec<ScheduledPublish>, ScheduledPublishError>> + Send;
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
    match tree.publish(
        ConfigLevel::Store,
        store_layer,
        version_id,
        vec![publish.node_key.clone()],
    ) {
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

/// Due rows, reordered so each `(release, store)` group applies in dependency order.
///
/// The store's `due` read sorts by effective time, which is the right order for unrelated rows and
/// the wrong one within a release: a Tết release carrying both `tax` and `menu` gives them the same
/// instant, and applying them as they were typed publishes the menu against last year's tax table
/// until the next write lands ([ADR-0125](../../../docs/adr/0125-a-release-is-one-decision-many-writes.md)
/// §6).
///
/// Grouped by `(release, store)` rather than by release alone, because the prerequisite is a fact
/// about one store's config tree: Ginza's `menu` waits on Ginza's `tax`, not on Hanoi's. Rows with no
/// release keep their `due` order untouched — ADR-0077's standalone schedules are one node each and
/// have nothing to order against.
fn in_dependency_order(due: Vec<ScheduledPublish>) -> Vec<ScheduledPublish> {
    let mut standalone: Vec<ScheduledPublish> = Vec::new();
    // Insertion-ordered, so groups keep the `due` order they arrived in and the pass stays
    // deterministic for a reader comparing two runs.
    let mut groups: Vec<((String, StoreId), Vec<ScheduledPublish>)> = Vec::new();
    for row in due {
        let Some(release_id) = row.release_id.clone() else {
            standalone.push(row);
            continue;
        };
        let key = (release_id, row.store_id);
        if let Some((_, rows)) = groups.iter_mut().find(|(existing, _)| *existing == key) {
            rows.push(row);
        } else {
            groups.push((key, vec![row]));
        }
    }
    let mut ordered = standalone;
    for (_, rows) in groups {
        let nodes: Vec<String> = rows.iter().map(|row| row.node_key.clone()).collect();
        // Each ordered name takes a row that has not been placed yet. Matching on the node alone
        // would hand the same row back for a group naming one node twice — one row applied twice
        // and the other silently dropped — which is a shape the ordering must not be able to
        // create, however unreachable the schedule route makes it.
        let mut placed = vec![false; rows.len()];
        for node in crate::releases::order_for_release(&nodes) {
            if let Some(at) = rows
                .iter()
                .enumerate()
                .position(|(at, row)| !placed[at] && row.node_key == node)
            {
                placed[at] = true;
                ordered.push(rows[at].clone());
            }
        }
    }
    ordered
}

/// How one release's pairs stand, counted from their rows.
///
/// A pair that *failed* to apply is still `Pending` — the activator retries it, and the reason sits
/// in `failure` for the report to read — so it is counted as pending here, not as a loss. What
/// counts against a release is a pair that will never publish, which is a cancelled one
/// ([ADR-0125](../../../docs/adr/0125-a-release-is-one-decision-many-writes.md) §5: cancelling
/// withdraws the still-pending rows and leaves the applied ones alone, and that shape is a
/// `partial`).
fn tally_pairs(pairs: &[ScheduledPublish]) -> PairTally {
    let mut tally = PairTally {
        pending: 0,
        applied: 0,
        failed: 0,
    };
    for pair in pairs {
        match pair.status {
            ScheduledPublishStatus::Pending => tally.pending += 1,
            ScheduledPublishStatus::Applied => tally.applied += 1,
            ScheduledPublishStatus::Cancelled => tally.failed += 1,
        }
    }
    tally
}

/// Re-derives every in-flight release's state from its own pairs, and records the ones that moved.
///
/// The release row's `status` is a stored roll-up, not a second source of truth: it exists so the
/// console's list does not have to aggregate N × M pair rows per release, and this is the one writer
/// that keeps it honest. Derived rather than asserted, so a release cannot claim `applied` while a
/// pair of its own says otherwise ([ADR-0125](../../../docs/adr/0125-a-release-is-one-decision-many-writes.md)
/// §4).
///
/// Only forward. A release `in_flight` with no pairs at all derives as `draft`, and writing that back
/// would un-schedule a release an operator has already approved — it is a bug in whatever emptied the
/// pairs, so it is logged and the row is left alone rather than quietly rewound.
///
/// A release whose roll-up cannot be read or written is logged and skipped, not propagated: the pass
/// that applied the publishes has already succeeded, and failing it here would re-apply nothing and
/// report the activator as broken.
async fn settle_releases<S, R>(scheduled: &S, releases: &R) -> Result<usize, String>
where
    S: ScheduledPublishStore,
    R: ReleaseStore,
{
    let in_flight = releases
        .in_flight()
        .await
        .map_err(|error| error.to_string())?;
    let mut moved = 0_usize;
    for release in in_flight {
        let pairs = match scheduled
            .list_for_release(release.tenant_id, &release.id)
            .await
        {
            Ok(pairs) => pairs,
            Err(error) => {
                tracing::error!(%error, release = %release.id, "could not read a release's pairs to settle it");
                continue;
            }
        };
        let derived = status_from_tally(tally_pairs(&pairs));
        if derived == release.status {
            continue;
        }
        if derived == ReleaseStatus::Draft {
            tracing::error!(
                release = %release.id,
                "an in-flight release has no pairs; leaving its recorded state alone"
            );
            continue;
        }
        match releases
            .set_status(release.tenant_id, &release.id, derived)
            .await
        {
            Ok(true) => {
                tracing::info!(release = %release.id, status = derived.as_wire(), "a release moved state");
                moved += 1;
            }
            Ok(false) => {
                tracing::warn!(release = %release.id, "a release vanished while being settled");
            }
            Err(error) => {
                tracing::error!(%error, release = %release.id, "could not record a release's new state");
            }
        }
    }
    Ok(moved)
}

/// One activator pass: applies every publish whose time has come. A row that fails to apply records
/// why on itself and is left pending to retry on the next tick, so one bad snapshot never blocks the
/// others. Returns how many were applied.
async fn pass<S, Cfg, R>(
    scheduled: &S,
    config_trees: &Cfg,
    releases: &R,
    now_ms: i64,
) -> Result<usize, String>
where
    S: ScheduledPublishStore,
    Cfg: ConfigTreeStore,
    R: ReleaseStore,
{
    let due = scheduled
        .due(now_ms)
        .await
        .map_err(|error| error.to_string())?;
    let mut applied = 0_usize;
    for publish in in_dependency_order(due) {
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
                // The reason goes on the row as well as into the log, because a release's report is
                // read from its pairs and a failure only the server can see is a release that looks
                // like it is still trying.
                if let Err(recording) = scheduled.mark_failed(&publish.id, &error).await {
                    tracing::error!(error = %recording, id = %publish.id, "could not record why a scheduled publish failed");
                }
            }
        }
    }
    // After the writes, not before: a release's state is derived from its pairs, so it is read once
    // they have settled. A release whose last pair applied on this very pass reaches `applied` in
    // the same tick rather than looking like it is still going until the next one.
    settle_releases(scheduled, releases).await?;
    Ok(applied)
}

/// Runs the scheduled-publish activator until `shutdown` resolves. Each tick applies every publish
/// whose effective time has arrived, re-derives every in-flight release's state from its pairs, and
/// records its own health — the same supervised-loop shape as the retention and alert loops.
///
/// It stays one loop. A release is bookkeeping over rows this activator already walks
/// ([ADR-0125](../../../docs/adr/0125-a-release-is-one-decision-many-writes.md) §1), so a second
/// loop would only add a way for the two to disagree about what has published.
pub async fn run<S, Cfg, R, Th, C>(
    scheduled: S,
    config_trees: Cfg,
    releases: R,
    task_health: Th,
    clock: C,
    interval: Duration,
    shutdown: impl Future<Output = ()>,
) where
    S: ScheduledPublishStore + Sync,
    Cfg: ConfigTreeStore + Sync,
    R: ReleaseStore + Sync,
    Th: TaskHealthStore + Sync,
    C: ClockSource,
{
    tokio::pin!(shutdown);
    loop {
        let now = clock.now();
        let detail = match pass(
            &scheduled,
            &config_trees,
            &releases,
            now.as_milliseconds_since_epoch(),
        )
        .await
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
    use crate::releases::{
        NewRelease, Release, ReleaseMoment, ReleaseStatus, ReleaseStore, ReleaseStoreError,
    };

    #[derive(Default)]
    struct FakeReleases {
        rows: Mutex<Vec<Release>>,
    }

    impl FakeReleases {
        /// A release already scheduled, so the roll-up has something in flight to settle.
        fn scheduled(id: &str, tenant_id: TenantId) -> Self {
            Self {
                rows: Mutex::new(vec![Release {
                    id: id.to_owned(),
                    tenant_id,
                    name: "Tết".to_owned(),
                    status: ReleaseStatus::Scheduled,
                    target_group_id: None,
                    moment: Some(ReleaseMoment::Instant(5_000)),
                    created_by: "admin".to_owned(),
                    created_at_ms: 0,
                    updated_at_ms: 0,
                }]),
            }
        }

        fn status_of(&self, id: &str) -> Option<ReleaseStatus> {
            self.rows
                .lock()
                .expect("lock")
                .iter()
                .find(|row| row.id == id)
                .map(|row| row.status)
        }
    }

    impl ReleaseStore for FakeReleases {
        async fn create(&self, release: &NewRelease) -> Result<(), ReleaseStoreError> {
            self.rows.lock().expect("lock").push(Release {
                id: release.id.clone(),
                tenant_id: release.tenant_id,
                name: release.name.clone(),
                status: ReleaseStatus::Draft,
                target_group_id: release.target_group_id.clone(),
                moment: release.moment.clone(),
                created_by: release.created_by.clone(),
                created_at_ms: 0,
                updated_at_ms: 0,
            });
            Ok(())
        }

        async fn list_for_tenant(
            &self,
            tenant_id: TenantId,
        ) -> Result<Vec<Release>, ReleaseStoreError> {
            Ok(self
                .rows
                .lock()
                .expect("lock")
                .iter()
                .filter(|row| row.tenant_id == tenant_id)
                .cloned()
                .collect())
        }

        async fn fetch_one(
            &self,
            tenant_id: TenantId,
            id: &str,
        ) -> Result<Option<Release>, ReleaseStoreError> {
            Ok(self
                .rows
                .lock()
                .expect("lock")
                .iter()
                .find(|row| row.tenant_id == tenant_id && row.id == id)
                .cloned())
        }

        async fn set_status(
            &self,
            tenant_id: TenantId,
            id: &str,
            status: ReleaseStatus,
        ) -> Result<bool, ReleaseStoreError> {
            let mut rows = self.rows.lock().expect("lock");
            let Some(row) = rows
                .iter_mut()
                .find(|row| row.tenant_id == tenant_id && row.id == id)
            else {
                return Ok(false);
            };
            row.status = status;
            Ok(true)
        }

        async fn in_flight(&self) -> Result<Vec<Release>, ReleaseStoreError> {
            Ok(self
                .rows
                .lock()
                .expect("lock")
                .iter()
                .filter(|row| {
                    matches!(
                        row.status,
                        ReleaseStatus::Scheduled | ReleaseStatus::Applying
                    )
                })
                .cloned()
                .collect())
        }
    }

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
                release_id: publish.release_id.clone(),
                failure: None,
                resolved_timezone: publish.resolved_timezone.clone(),
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
                    // The real adapter clears the column on success, so a row that failed once and
                    // then landed does not keep explaining itself. A fake that only set it would let
                    // a stale-reason bug pass.
                    row.failure = None;
                }
            }
            Ok(())
        }

        async fn mark_failed(&self, id: &str, failure: &str) -> Result<(), ScheduledPublishError> {
            let mut rows = self.rows.lock().expect("lock");
            for row in rows.iter_mut() {
                if row.id == id && row.status == ScheduledPublishStatus::Pending {
                    row.failure = Some(failure.to_owned());
                }
            }
            Ok(())
        }

        async fn list_for_release(
            &self,
            tenant_id: TenantId,
            release_id: &str,
        ) -> Result<Vec<ScheduledPublish>, ScheduledPublishError> {
            Ok(self
                .rows
                .lock()
                .expect("lock")
                .iter()
                .filter(|row| {
                    row.tenant_id == tenant_id && row.release_id.as_deref() == Some(release_id)
                })
                .cloned()
                .collect())
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
            release_id: None,
            resolved_timezone: None,
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
        /// When set, every `save` reports a lost race. That is the shape of a real failure the
        /// activator is built to survive — the row stays pending and the next pass retries — and it
        /// is the one a test can produce without a broken snapshot.
        refuse_saves: bool,
    }

    impl FakeConfigTrees {
        /// A tree store whose every write loses the race.
        fn failing() -> Self {
            Self {
                states: Mutex::default(),
                refuse_saves: true,
            }
        }
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
            if self.refuse_saves {
                return Ok(crate::version::UpdateOutcome::VersionMismatch);
            }
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
            _print_agents: Option<Vec<crate::fleet::PrintAgentStanding>>,
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
        let applied = super::pass(&scheduled, &config_trees, &FakeReleases::default(), 5_000)
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
            super::pass(&scheduled, &config_trees, &FakeReleases::default(), 5_000)
                .await
                .expect("second pass"),
            0
        );
    }
    /// A release pair, for the ordering tests. Same instant for every node, which is the case that
    /// makes ordering load-bearing: `due` sorts by effective time and a release gives them all one.
    fn release_row(node: &str, release: Option<&str>, store_seed: u128) -> ScheduledPublish {
        ScheduledPublish {
            id: format!("{}-{node}", release.unwrap_or("solo")),
            tenant_id: tenant(),
            store_id: StoreId::new(Ulid::from_u128(store_seed)),
            node_key: node.to_owned(),
            node_value: serde_json::json!({}),
            effective_at_ms: 1_000,
            status: ScheduledPublishStatus::Pending,
            created_at_ms: 0,
            applied_version_id: None,
            release_id: release.map(str::to_owned),
            failure: None,
            resolved_timezone: None,
        }
    }

    #[test]
    fn a_pass_applies_a_release_tax_before_its_menu() {
        // The failure: `due` sorts by effective time, a release gives every pair the same instant,
        // and the menu therefore lands whenever it happened to be typed — against last year's tax
        // table until the next write.
        let due = vec![
            release_row("menu", Some("tet"), 2),
            release_row("tax", Some("tet"), 2),
        ];
        let ordered = super::in_dependency_order(due);
        let nodes: Vec<&str> = ordered.iter().map(|row| row.node_key.as_str()).collect();
        assert_eq!(nodes, vec!["tax", "menu"]);
    }

    #[test]
    fn a_group_naming_one_node_twice_keeps_both_rows() {
        // Not a shape the schedule route can produce — a release writes one row per (node, store) —
        // but the ordering must not be the thing that loses a row if one ever arrives.
        let due = vec![
            release_row("tax", Some("tet"), 2),
            ScheduledPublish {
                id: "tet-tax-again".to_owned(),
                ..release_row("tax", Some("tet"), 2)
            },
        ];
        let ordered = super::in_dependency_order(due);
        let ids: Vec<&str> = ordered.iter().map(|row| row.id.as_str()).collect();
        assert_eq!(ids, vec!["tet-tax", "tet-tax-again"]);
    }

    #[test]
    fn the_prerequisite_is_per_store_not_per_release() {
        // Ginza's menu waits on Ginza's tax, not on Hanoi's: the prerequisite is a fact about one
        // store's config tree. Grouping by release alone would let one store's tax satisfy another
        // store's menu and publish a menu against a table that shop does not hold.
        let due = vec![
            release_row("menu", Some("tet"), 2),
            release_row("tax", Some("tet"), 3),
        ];
        let ordered = super::in_dependency_order(due);
        assert_eq!(ordered.len(), 2, "no pair is dropped");
        // Neither store carries both, so neither is held back — each applies on its own terms.
        for row in &ordered {
            assert_eq!(row.release_id.as_deref(), Some("tet"));
        }
    }

    #[test]
    fn a_standalone_schedule_keeps_the_order_the_store_gave_it() {
        // ADR-0077's per-store schedule is one node at a time and has nothing to order against;
        // reordering it would be a behaviour change this slice has no reason to make.
        let due = vec![
            release_row("menu", None, 2),
            release_row("campaigns", None, 2),
        ];
        let ordered = super::in_dependency_order(due);
        let nodes: Vec<&str> = ordered.iter().map(|row| row.node_key.as_str()).collect();
        assert_eq!(nodes, vec!["menu", "campaigns"]);
    }

    #[tokio::test]
    async fn a_publish_that_cannot_apply_records_why_on_its_own_row() {
        // A failure only the server log can see is a release that reports itself as still trying.
        // The row stays pending — the activator retries — but it now says what went wrong.
        let scheduled = FakeScheduled::default();
        let config_trees = FakeConfigTrees::failing();
        scheduled
            .schedule(&new_publish("doomed-1", 1_000))
            .await
            .expect("schedule");

        let applied = super::pass(&scheduled, &config_trees, &FakeReleases::default(), 5_000)
            .await
            .expect("a pass whose writes all fail is not itself a failure");
        assert_eq!(applied, 0);

        let rows = scheduled
            .list_for_store(tenant(), store())
            .await
            .expect("list");
        let row = rows
            .first()
            .expect("the row is still pending, to be retried");
        assert_eq!(row.status, ScheduledPublishStatus::Pending);
        assert!(
            row.failure.is_some(),
            "the reason is on the row, where a release's report reads it"
        );
    }

    /// A pair belonging to a release: one node at one shop, which is the shape a release fans out to.
    ///
    /// Each pair gets its own store, because that is what a release is — the same node at N shops —
    /// and because two pairs naming one node at one store is a set a release cannot produce.
    fn release_publish(
        id: &str,
        release: &str,
        store_seed: u128,
        effective_at_ms: i64,
    ) -> NewScheduledPublish {
        NewScheduledPublish {
            store_id: StoreId::new(Ulid::from_u128(store_seed)),
            release_id: Some(release.to_owned()),
            ..new_publish(id, effective_at_ms)
        }
    }

    #[tokio::test]
    async fn a_release_reaches_applied_on_the_pass_that_lands_its_last_pair() {
        let scheduled = FakeScheduled::default();
        let config_trees = FakeConfigTrees::default();
        let releases = FakeReleases::scheduled("tet", tenant());
        for (id, shop) in [("tet-ginza", 1_u128), ("tet-hanoi", 2)] {
            scheduled
                .schedule(&release_publish(id, "tet", shop, 1_000))
                .await
                .expect("schedule");
        }

        super::pass(&scheduled, &config_trees, &releases, 5_000)
            .await
            .expect("pass");

        assert_eq!(
            releases.status_of("tet"),
            Some(ReleaseStatus::Applied),
            "the roll-up is read after the writes, so a release settles in the same tick its last \
             pair lands rather than looking like it is still going until the next one"
        );
    }

    #[tokio::test]
    async fn a_release_with_a_pair_still_waiting_is_applying_not_applied() {
        let scheduled = FakeScheduled::default();
        let config_trees = FakeConfigTrees::default();
        let releases = FakeReleases::scheduled("tet", tenant());
        scheduled
            .schedule(&release_publish("tet-now", "tet", 1, 1_000))
            .await
            .expect("schedule the ripe pair");
        scheduled
            .schedule(&release_publish("tet-later", "tet", 2, 9_000))
            .await
            .expect("schedule the pair whose instant has not come");

        super::pass(&scheduled, &config_trees, &releases, 5_000)
            .await
            .expect("pass");

        // The distinction the state exists for: 360 writes are not instantaneous, and a console
        // showing `scheduled` while the activator is halfway through is lying to an operator who
        // may be watching a switchover (ADR-0125 §4).
        assert_eq!(releases.status_of("tet"), Some(ReleaseStatus::Applying));
    }

    #[tokio::test]
    async fn a_release_whose_pair_was_cancelled_after_another_applied_is_partial() {
        let scheduled = FakeScheduled::default();
        let config_trees = FakeConfigTrees::default();
        let releases = FakeReleases::scheduled("tet", tenant());
        scheduled
            .schedule(&release_publish("tet-ginza", "tet", 1, 1_000))
            .await
            .expect("schedule");
        scheduled
            .schedule(&release_publish("tet-hanoi", "tet", 2, 1_000))
            .await
            .expect("schedule");
        assert!(
            scheduled
                .cancel(tenant(), "tet-hanoi")
                .await
                .expect("cancel"),
            "one shop is pulled out before the release fires"
        );

        super::pass(&scheduled, &config_trees, &releases, 5_000)
            .await
            .expect("pass");

        // Not `applied`, which would hide the shop that did not take it, and not `failed`, which
        // would hide the one that did. At forty stores the operator needs the two that are missing.
        assert_eq!(releases.status_of("tet"), Some(ReleaseStatus::Partial));
    }

    #[tokio::test]
    async fn a_failed_pair_keeps_its_release_going_rather_than_ending_it() {
        let scheduled = FakeScheduled::default();
        let config_trees = FakeConfigTrees::failing();
        let releases = FakeReleases::scheduled("tet", tenant());
        scheduled
            .schedule(&release_publish("tet-a", "tet", 1, 1_000))
            .await
            .expect("schedule");

        super::pass(&scheduled, &config_trees, &releases, 5_000)
            .await
            .expect("pass");

        // A pair that could not apply stays pending and is retried, so the release is still
        // waiting — not `partial`. `partial` is terminal, and calling a retry terminal would tell
        // an operator to go and fix by hand something the next tick is about to do.
        assert_eq!(releases.status_of("tet"), Some(ReleaseStatus::Scheduled));
    }

    #[tokio::test]
    async fn an_in_flight_release_with_no_pairs_keeps_the_state_it_was_given() {
        let scheduled = FakeScheduled::default();
        let config_trees = FakeConfigTrees::default();
        let releases = FakeReleases::scheduled("tet", tenant());

        super::pass(&scheduled, &config_trees, &releases, 5_000)
            .await
            .expect("pass");

        // An empty tally derives as `draft`, and writing that back would un-schedule a release an
        // operator has already approved. The roll-up only moves a release forward.
        assert_eq!(releases.status_of("tet"), Some(ReleaseStatus::Scheduled));
    }
}
