// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! Store groups and the batches published to them
//! ([ADR-0122](../../../docs/adr/0122-a-store-group-is-a-delivery-cohort.md)).
//!
//! # What a group is, and what it is not
//!
//! A **store group** is a named, flat, tenant-scoped set of stores that an operator publishes to as
//! one. It holds no configuration of its own. A batch publish calls the same per-store publish once
//! per member and writes the same document into *N* config trees, exactly as *N* individual
//! publishes would have — so [ADR-0033](../../../docs/adr/0033-config-tree.md)'s four layers,
//! its composition, and the store's sync hot path are all untouched, and deleting every group
//! returns the trees byte-for-byte to what they are without one.
//!
//! It is deliberately **not the brand**. `stores.brand_id` already exists and is *identity* — which
//! sign is over the door — and a store has exactly one. The sets an operator publishes to cut across
//! that: the airport branches on a reduced menu, the six shops on one tax registration, the three
//! pilots that take a change first. A store belongs to one brand and any number of groups.
//!
//! # Why the batch record is a table and not a log line
//!
//! A batch is not atomic (ADR-0122 §5): each store's tree is its own row, so an all-or-nothing
//! fan-out would hold *N* locks and fail as "nothing happened, at 200 shops, because of one". A
//! partial success is the honest outcome, and re-running is idempotent for the stores that already
//! succeeded because a publish producing an identical document is a no-op version.
//!
//! What must *not* be partial is the reporting, which is what [`BatchResult`] is for: one durable
//! row per `(batch, store)`, so "did every shop get the new price?" is a query and not twenty
//! screens. There are **three** outcomes and not two, and that is the load-bearing part — a
//! two-state report files a store that was deliberately protected under the same heading as a store
//! that broke.

use core::fmt;
use core::future::Future;

use pos_proto::ids::{StoreId, TenantId};
use pos_proto::ulid::Ulid;
use serde::Serialize;

use crate::registry::EntityStatus;
use crate::version::{UpdateOutcome, Version, Versioned};

/// A store group's identifier — a ULID minted when the group is created.
///
/// Like [`MenuId`](crate::catalog::MenuId) it never crosses the store wire: a group is an authoring
/// and delivery concept in the cloud, and what reaches a store is the same config node it would have
/// received from a single publish.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(transparent)]
pub struct StoreGroupId(Ulid);

impl StoreGroupId {
    /// Wraps a ULID as a store-group id.
    #[must_use]
    pub const fn new(id: Ulid) -> Self {
        Self(id)
    }

    /// The underlying ULID.
    #[must_use]
    pub const fn as_ulid(self) -> Ulid {
        self.0
    }
}

impl fmt::Display for StoreGroupId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// A batch id — a ULID minted when the fan-out starts, and the handle every result carries.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(transparent)]
pub struct ConfigBatchId(Ulid);

impl ConfigBatchId {
    /// Wraps a ULID as a batch id.
    #[must_use]
    pub const fn new(id: Ulid) -> Self {
        Self(id)
    }

    /// The underlying ULID.
    #[must_use]
    pub const fn as_ulid(self) -> Ulid {
        self.0
    }
}

impl fmt::Display for ConfigBatchId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// A cohort of stores an operator publishes to as one.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct StoreGroup {
    /// The group's id.
    pub group_id: StoreGroupId,
    /// The owning tenant.
    pub tenant_id: TenantId,
    /// The human name — "Airport", "VN-South", "Pilot".
    pub name: String,
    /// Active or archived. An archived group keeps its membership (so the record of what it *was*
    /// survives) and is refused as a batch target.
    pub status: EntityStatus,
}

/// What a batch publish did at one store.
///
/// Three variants, because there are three real answers to "what happened at this shop", and the
/// distinction between the first two is the whole reason the report exists.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum BatchOutcome {
    /// The publish ran and produced a config version.
    Applied,
    /// Nothing was attempted, on purpose: the store is archived, or its tree is missing a node this
    /// publish depends on. A `skipped` store is protected, not broken, and the detail says which.
    Skipped,
    /// The publish was attempted and refused. This is the one that needs a person.
    Failed,
}

impl BatchOutcome {
    /// The wire token, matching the `config_batch_results_outcome` CHECK.
    #[must_use]
    pub const fn as_wire(self) -> &'static str {
        match self {
            Self::Applied => "applied",
            Self::Skipped => "skipped",
            Self::Failed => "failed",
        }
    }

    /// Parses a stored token, or `None` for one this build does not know.
    #[must_use]
    pub fn from_wire(token: &str) -> Option<Self> {
        match token {
            "applied" => Some(Self::Applied),
            "skipped" => Some(Self::Skipped),
            "failed" => Some(Self::Failed),
            _ => None,
        }
    }
}

/// One fan-out: what was published, to which group, by whom, and when.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ConfigBatch {
    /// The batch's id.
    pub batch_id: ConfigBatchId,
    /// The owning tenant.
    pub tenant_id: TenantId,
    /// The group it fanned out over.
    pub group_id: StoreGroupId,
    /// The node kind published (`"menu"`, `"tax"`, `"capabilities"`, …).
    pub node: String,
    /// The node's arguments exactly as the operator gave them, so the batch can be read back and
    /// understood a month later without reconstructing what the screen looked like.
    pub arguments: serde_json::Value,
    /// The console admin who pressed the button — the same identity the audit trail records.
    pub actor_email: String,
    /// When the fan-out started, in milliseconds since the epoch.
    pub started_at_ms: i64,
    /// When it finished. `None` for a batch that died mid-fan-out, which is exactly the row an
    /// operator needs to be able to find.
    pub finished_at_ms: Option<i64>,
}

/// One `(batch, store)` row of the report.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct BatchResult {
    /// The store this row is about.
    pub store_id: StoreId,
    /// What happened there.
    pub outcome: BatchOutcome,
    /// The refusal's own message, for `skipped` and `failed`. `None` when it applied.
    pub detail: Option<String>,
    /// The config version the publish produced, for `applied`. Absent otherwise — and that absence
    /// is what makes drift visible: a member whose last batch produced no version is not running
    /// what its group runs.
    pub version_id: Option<String>,
    /// When this store was reached, in milliseconds since the epoch.
    pub at_ms: i64,
}

/// A store-group persistence failure.
///
/// One variant, like the other cloud store seams: every failure here is "the database did not
/// answer", and a caller's only sane response is the same `503` in each case. Splitting it would
/// invite a handler to branch on a distinction it cannot act on.
#[derive(Debug, thiserror::Error)]
#[error("the store-group store is unavailable: {0}")]
pub struct StoreGroupStoreError(pub String);

/// Persists and reads a tenant's store groups and the batches published to them.
///
/// Every method is tenant-scoped; the `store-postgres` impl is RLS-isolated by tenant like every
/// other cloud table.
///
/// # Why membership shares the group's version
///
/// [`set_members`](Self::set_members) is a wholesale replace under the group's own [`Version`] —
/// [ADR-0095](../../../docs/adr/0095-conditional-writes-for-collections.md)'s shape C. Membership is
/// a *set*, and two admins editing a cohort concurrently is exactly the collision ADR-0094 exists
/// for: without the precondition, the second save silently drops whatever the first added. Giving
/// the set the group's version rather than a version of its own means one token covers "the group
/// as the screen last read it", which is what the screen actually holds.
pub trait StoreGroupStore {
    /// Inserts a group, returning the [`Version`] it starts at.
    fn create_group(
        &self,
        group: &StoreGroup,
    ) -> impl Future<Output = Result<Version, StoreGroupStoreError>> + Send;

    /// A tenant's groups, id order (a ULID, so creation order), including archived ones.
    fn list_groups(
        &self,
        tenant_id: TenantId,
    ) -> impl Future<Output = Result<Vec<Versioned<StoreGroup>>, StoreGroupStoreError>> + Send;

    /// Renames a group and/or sets its status. Applies only at `expected`.
    fn update_group(
        &self,
        group: &StoreGroup,
        expected: &Version,
    ) -> impl Future<Output = Result<UpdateOutcome, StoreGroupStoreError>> + Send;

    /// A group's members, store-id order.
    fn list_members(
        &self,
        tenant_id: TenantId,
        group_id: StoreGroupId,
    ) -> impl Future<Output = Result<Vec<StoreId>, StoreGroupStoreError>> + Send;

    /// Replaces a group's membership wholesale, at `expected` (see the note on this trait).
    fn set_members(
        &self,
        tenant_id: TenantId,
        group_id: StoreGroupId,
        members: &[StoreId],
        expected: &Version,
    ) -> impl Future<Output = Result<UpdateOutcome, StoreGroupStoreError>> + Send;

    /// Records that a fan-out has begun, before the first store is touched.
    ///
    /// Written first and finished last on purpose: a batch that dies half-way leaves a row with a
    /// `None` finish and however many results it managed, which is a recoverable state an operator
    /// can read. A batch recorded only on success would leave that same half-applied fleet with no
    /// record at all.
    fn start_batch(
        &self,
        batch: &ConfigBatch,
    ) -> impl Future<Output = Result<(), StoreGroupStoreError>> + Send;

    /// Records what happened at one store. Idempotent on `(batch, store)`: a re-run of the same
    /// batch id overwrites the row rather than doubling it.
    fn record_result(
        &self,
        tenant_id: TenantId,
        batch_id: ConfigBatchId,
        result: &BatchResult,
    ) -> impl Future<Output = Result<(), StoreGroupStoreError>> + Send;

    /// Stamps a batch finished.
    fn finish_batch(
        &self,
        tenant_id: TenantId,
        batch_id: ConfigBatchId,
        finished_at_ms: i64,
    ) -> impl Future<Output = Result<(), StoreGroupStoreError>> + Send;

    /// A group's batches, newest first, at most `limit`.
    fn list_batches(
        &self,
        tenant_id: TenantId,
        group_id: StoreGroupId,
        limit: u32,
    ) -> impl Future<Output = Result<Vec<ConfigBatch>, StoreGroupStoreError>> + Send;

    /// One batch's per-store report, store-id order.
    fn batch_results(
        &self,
        tenant_id: TenantId,
        batch_id: ConfigBatchId,
    ) -> impl Future<Output = Result<Vec<BatchResult>, StoreGroupStoreError>> + Send;
}

/// An in-memory [`StoreGroupStore`] for this module's own tests.
///
/// `#[cfg(test)]` and not a feature, matching `reason_codes` and the rest: the integration suite in
/// `tests/cloud.rs` builds its own fakes against the same trait, so a fake compiled into the shipped
/// binary would be a store nothing reaches carried by every deployment.
#[cfg(test)]
mod fake {
    use std::collections::BTreeMap;
    use std::sync::{Arc, Mutex};

    use super::{
        BatchResult, ConfigBatch, ConfigBatchId, StoreGroup, StoreGroupId, StoreGroupStore,
        StoreGroupStoreError,
    };
    use crate::version::{UpdateOutcome, Version, Versioned};
    use pos_proto::ids::{StoreId, TenantId};

    /// The whole state, behind one lock — the same shape the other cloud fakes use.
    #[derive(Debug, Default)]
    struct State {
        groups: BTreeMap<(TenantId, StoreGroupId), (StoreGroup, Version)>,
        members: BTreeMap<(TenantId, StoreGroupId), Vec<StoreId>>,
        batches: BTreeMap<(TenantId, ConfigBatchId), ConfigBatch>,
        results: BTreeMap<(TenantId, ConfigBatchId, StoreId), BatchResult>,
    }

    /// A store-group store held in memory.
    #[derive(Debug, Clone, Default)]
    pub(super) struct FakeStoreGroups {
        state: Arc<Mutex<State>>,
    }

    /// The next version after `at`, as an opaque counter. The real adapter uses Postgres `xmin`
    /// (ADR-0094); a fake only has to be *different* after a write, and monotone so a stale token is
    /// distinguishable from a fresh one.
    fn bump(at: &Version) -> Version {
        let next: u64 = at.as_str().parse::<u64>().unwrap_or(0) + 1;
        Version::new(next.to_string())
    }

    impl StoreGroupStore for FakeStoreGroups {
        async fn create_group(&self, group: &StoreGroup) -> Result<Version, StoreGroupStoreError> {
            let mut state = self.state.lock().expect("lock");
            let at = Version::new("1".to_owned());
            state.groups.insert(
                (group.tenant_id, group.group_id),
                (group.clone(), at.clone()),
            );
            state
                .members
                .insert((group.tenant_id, group.group_id), Vec::new());
            Ok(at)
        }

        async fn list_groups(
            &self,
            tenant_id: TenantId,
        ) -> Result<Vec<Versioned<StoreGroup>>, StoreGroupStoreError> {
            let state = self.state.lock().expect("lock");
            Ok(state
                .groups
                .iter()
                .filter(|((owner, _), _)| *owner == tenant_id)
                .map(|(_, (group, at))| Versioned::new(group.clone(), at.clone()))
                .collect())
        }

        async fn update_group(
            &self,
            group: &StoreGroup,
            expected: &Version,
        ) -> Result<UpdateOutcome, StoreGroupStoreError> {
            let mut state = self.state.lock().expect("lock");
            let Some((held, at)) = state.groups.get_mut(&(group.tenant_id, group.group_id)) else {
                return Ok(UpdateOutcome::NotFound);
            };
            if at != expected {
                return Ok(UpdateOutcome::VersionMismatch);
            }
            *held = group.clone();
            *at = bump(at);
            Ok(UpdateOutcome::Updated(at.clone()))
        }

        async fn list_members(
            &self,
            tenant_id: TenantId,
            group_id: StoreGroupId,
        ) -> Result<Vec<StoreId>, StoreGroupStoreError> {
            let state = self.state.lock().expect("lock");
            let mut mine = state
                .members
                .get(&(tenant_id, group_id))
                .cloned()
                .unwrap_or_default();
            mine.sort_by_key(|store| store.as_ulid().to_u128());
            Ok(mine)
        }

        async fn set_members(
            &self,
            tenant_id: TenantId,
            group_id: StoreGroupId,
            members: &[StoreId],
            expected: &Version,
        ) -> Result<UpdateOutcome, StoreGroupStoreError> {
            let mut state = self.state.lock().expect("lock");
            let Some((_, at)) = state.groups.get_mut(&(tenant_id, group_id)) else {
                return Ok(UpdateOutcome::NotFound);
            };
            if at != expected {
                return Ok(UpdateOutcome::VersionMismatch);
            }
            *at = bump(at);
            let next = at.clone();
            state
                .members
                .insert((tenant_id, group_id), members.to_vec());
            Ok(UpdateOutcome::Updated(next))
        }

        async fn start_batch(&self, batch: &ConfigBatch) -> Result<(), StoreGroupStoreError> {
            let mut state = self.state.lock().expect("lock");
            state
                .batches
                .insert((batch.tenant_id, batch.batch_id), batch.clone());
            Ok(())
        }

        async fn record_result(
            &self,
            tenant_id: TenantId,
            batch_id: ConfigBatchId,
            result: &BatchResult,
        ) -> Result<(), StoreGroupStoreError> {
            let mut state = self.state.lock().expect("lock");
            state
                .results
                .insert((tenant_id, batch_id, result.store_id), result.clone());
            Ok(())
        }

        async fn finish_batch(
            &self,
            tenant_id: TenantId,
            batch_id: ConfigBatchId,
            finished_at_ms: i64,
        ) -> Result<(), StoreGroupStoreError> {
            let mut state = self.state.lock().expect("lock");
            if let Some(batch) = state.batches.get_mut(&(tenant_id, batch_id)) {
                batch.finished_at_ms = Some(finished_at_ms);
            }
            Ok(())
        }

        async fn list_batches(
            &self,
            tenant_id: TenantId,
            group_id: StoreGroupId,
            limit: u32,
        ) -> Result<Vec<ConfigBatch>, StoreGroupStoreError> {
            let state = self.state.lock().expect("lock");
            let mut mine: Vec<ConfigBatch> = state
                .batches
                .values()
                .filter(|batch| batch.tenant_id == tenant_id && batch.group_id == group_id)
                .cloned()
                .collect();
            mine.sort_by(|a, b| b.started_at_ms.cmp(&a.started_at_ms));
            mine.truncate(limit as usize);
            Ok(mine)
        }

        async fn batch_results(
            &self,
            tenant_id: TenantId,
            batch_id: ConfigBatchId,
        ) -> Result<Vec<BatchResult>, StoreGroupStoreError> {
            let state = self.state.lock().expect("lock");
            Ok(state
                .results
                .iter()
                .filter(|((owner, batch, _), _)| *owner == tenant_id && *batch == batch_id)
                .map(|(_, result)| result.clone())
                .collect())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::fake::FakeStoreGroups;
    use super::*;

    fn tenant() -> TenantId {
        TenantId::new(Ulid::from_u128(1))
    }

    fn group(id: u128, name: &str) -> StoreGroup {
        StoreGroup {
            group_id: StoreGroupId::new(Ulid::from_u128(id)),
            tenant_id: tenant(),
            name: name.to_owned(),
            status: EntityStatus::Active,
        }
    }

    fn store(id: u128) -> StoreId {
        StoreId::new(Ulid::from_u128(id))
    }

    #[tokio::test]
    async fn a_new_group_starts_empty_rather_than_holding_every_store() {
        // The default matters: a group that started as "all stores" would make creating one an act
        // with a blast radius, and an operator naming a cohort has not yet said what is in it.
        let store_groups = FakeStoreGroups::default();
        store_groups
            .create_group(&group(10, "Airport"))
            .await
            .expect("create");
        let members = store_groups
            .list_members(tenant(), StoreGroupId::new(Ulid::from_u128(10)))
            .await
            .expect("members");
        assert!(members.is_empty());
    }

    #[tokio::test]
    async fn a_stale_membership_edit_is_refused_and_the_first_one_survives() {
        // Two admins editing a cohort is the collision ADR-0094 exists for. Without the precondition
        // the second save drops whatever the first added, silently and with both screens showing a
        // success.
        let store_groups = FakeStoreGroups::default();
        let both_read = store_groups
            .create_group(&group(10, "Airport"))
            .await
            .expect("create");
        let id = StoreGroupId::new(Ulid::from_u128(10));

        let first = store_groups
            .set_members(tenant(), id, &[store(100), store(101)], &both_read)
            .await
            .expect("the first edit");
        assert!(matches!(first, UpdateOutcome::Updated(_)));

        let second = store_groups
            .set_members(tenant(), id, &[store(200)], &both_read)
            .await
            .expect("the second edit");
        assert!(matches!(second, UpdateOutcome::VersionMismatch));

        assert_eq!(
            store_groups
                .list_members(tenant(), id)
                .await
                .expect("members"),
            vec![store(100), store(101)],
            "the refused write must not have applied"
        );
    }

    #[tokio::test]
    async fn a_batch_is_recorded_before_it_runs_and_stamped_when_it_ends() {
        // A batch that dies half-way must leave a readable record: the row with no finish, and the
        // results it managed. Recording only on success would leave a half-applied fleet with
        // nothing at all to read.
        let store_groups = FakeStoreGroups::default();
        let group_id = StoreGroupId::new(Ulid::from_u128(10));
        let batch_id = ConfigBatchId::new(Ulid::from_u128(77));
        let batch = ConfigBatch {
            batch_id,
            tenant_id: tenant(),
            group_id,
            node: "menu".to_owned(),
            arguments: serde_json::json!({ "menu_id": "01M2" }),
            actor_email: "ops@example.com".to_owned(),
            started_at_ms: 1_000,
            finished_at_ms: None,
        };
        store_groups.start_batch(&batch).await.expect("start");

        let held = &store_groups
            .list_batches(tenant(), group_id, 10)
            .await
            .expect("batches")[0];
        assert_eq!(held.finished_at_ms, None, "unfinished until it finishes");

        store_groups
            .record_result(
                tenant(),
                batch_id,
                &BatchResult {
                    store_id: store(100),
                    outcome: BatchOutcome::Applied,
                    detail: None,
                    version_id: Some("01V1".to_owned()),
                    at_ms: 1_100,
                },
            )
            .await
            .expect("result");
        store_groups
            .record_result(
                tenant(),
                batch_id,
                &BatchResult {
                    store_id: store(101),
                    outcome: BatchOutcome::Skipped,
                    detail: Some("this store's tree has no `tax` node".to_owned()),
                    version_id: None,
                    at_ms: 1_200,
                },
            )
            .await
            .expect("result");
        store_groups
            .finish_batch(tenant(), batch_id, 1_300)
            .await
            .expect("finish");

        let results = store_groups
            .batch_results(tenant(), batch_id)
            .await
            .expect("results");
        assert_eq!(results.len(), 2);
        // The skipped row keeps its reason. A report that said only "2 stores, 1 applied" would
        // leave the operator to work out which one and why.
        let skipped = results
            .iter()
            .find(|row| row.outcome == BatchOutcome::Skipped)
            .expect("a skipped row");
        assert!(
            skipped
                .detail
                .as_deref()
                .unwrap_or_default()
                .contains("tax")
        );
        assert_eq!(skipped.version_id, None);

        let finished = &store_groups
            .list_batches(tenant(), group_id, 10)
            .await
            .expect("batches")[0];
        assert_eq!(finished.finished_at_ms, Some(1_300));
    }

    #[test]
    fn every_outcome_round_trips_through_its_wire_token() {
        // The token is a CHECK constraint in migration 0061 and a discriminator the console reads,
        // so a variant that does not round-trip is a row the database refuses or a badge the screen
        // cannot draw.
        for outcome in [
            BatchOutcome::Applied,
            BatchOutcome::Skipped,
            BatchOutcome::Failed,
        ] {
            assert_eq!(BatchOutcome::from_wire(outcome.as_wire()), Some(outcome));
        }
        assert_eq!(BatchOutcome::from_wire("partially_applied"), None);
    }
}
