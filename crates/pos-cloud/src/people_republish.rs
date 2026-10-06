// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The stores whose people the cloud publishes again by itself, after a migration changes what
//! their roles grant ([ADR-0158](../../../docs/adr/0158-the-till-enforces-each-persons-own-permissions.md)).
//!
//! A store's `permissions` node is compiled from its people, roles and assignments when they are
//! published, by **Publish** on People or by a console edit that publishes at once. A migration
//! that changes what roles grant changes no store's node, so it queues the stores whose people have
//! been published (`people_republishes`, migration 0083, whose header states the convention a later
//! grant migration follows), and this module drains the queue: at start-up, and then every
//! [`DRAIN_INTERVAL`].
//!
//! Each queued store's people are published again the way **Publish** on People publishes them
//! ([`crate::http::republish_people`]): the same compile, the same validation, the same node,
//! recorded as `permissions.publish` under the system actor with the queue's reason beside the
//! version and the staff count, and never a name, a code or a PIN hash. A store whose node is
//! already what its people compile to is given no new version. A store whose configuration holds
//! no `permissions` roster (a `staff` list) on any layer, whose people were never published, is
//! given none; a rolled-back store, whose roster the rollback moved onto its Tenant layer, is
//! published again like any other. Either way its row is cleared. A store that cannot be
//! published keeps its row for the next drain, and the log names its id and the refusal's HTTP
//! status, nothing else.
//!
//! One store at a time, [`DRAIN_BATCH`] rows read at a time, in the queue's own order and past the
//! last row read, so a drain holds at most one batch in memory and reaches every row once however
//! many of them fail. One at a time because the publishes share the console's connection pool and
//! nobody waits on them: a migration queues at most one row per store, and each publish is a few
//! reads and one conditional write.

use core::future::Future;
use core::time::Duration;
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, PoisonError};

use pos_proto::ClockSource;
use pos_proto::ids::{ConfigVersionId, StoreId, TenantId};
use pos_proto::time::Timestamp;

use crate::audit::AuditRecorder;
use crate::config_tree::ConfigTreeStore;
use crate::health::{TaskHealthStore, tick_detail};
use crate::people::{AssignmentStore, EmployeeStore, RoleTemplateStore};

/// The canonical name of the people republisher loop, for task-health reporting.
pub const PEOPLE_REPUBLISHER: &str = "people_republisher";

/// How long the republisher waits between drains after the one at start-up. Only a migration
/// queues a store, and migrations run at start-up before the first drain, so a later drain mostly
/// retries the stores an earlier one could not publish: every five minutes is soon enough for that,
/// and costs one read when the queue is empty.
pub const DRAIN_INTERVAL: Duration = Duration::from_secs(5 * 60);

/// How many queued stores one read of the queue hands the drain.
pub const DRAIN_BATCH: usize = 64;

/// The most stores [`InMemoryPeopleRepublishes`] holds.
pub const IN_MEMORY_REPUBLISHES: usize = 4_096;

/// One store waiting for its people to be published again.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PeopleRepublish {
    /// The store's tenant.
    pub tenant_id: TenantId,
    /// The store.
    pub store_id: StoreId,
    /// The migration that queued it, by its file's name, which the publish records as its cause.
    pub reason: String,
    /// When it was queued, to the millisecond. A clear removes the row only while it still
    /// carries this time.
    pub enqueued_time: Timestamp,
}

/// Persists the queue of stores whose people the cloud publishes again by itself.
///
/// A store is queued at most once: queuing it again moves its reason and time forward. The reads
/// and the clear span every tenant, as the trusted owner the drain runs as (the scheduled
/// publishes' `due` read does the same), because the queue is fleet-wide.
pub trait PeopleRepublishStore {
    /// Queues a store, or moves the reason and time of its row forward if it is queued already.
    fn enqueue(
        &self,
        republish: &PeopleRepublish,
    ) -> impl Future<Output = Result<(), PeopleRepublishError>> + Send;

    /// Up to `limit` queued stores, across every tenant, in tenant then store order, starting past
    /// `after` when it is given.
    fn pending(
        &self,
        after: Option<(TenantId, StoreId)>,
        limit: usize,
    ) -> impl Future<Output = Result<Vec<PeopleRepublish>, PeopleRepublishError>> + Send;

    /// Removes a store's row while it still carries the time `republish` was read with, and says
    /// whether it removed one: a store queued again since stays queued.
    fn clear(
        &self,
        republish: &PeopleRepublish,
    ) -> impl Future<Output = Result<bool, PeopleRepublishError>> + Send;
}

/// A failure of the queue's store itself.
#[derive(Debug, thiserror::Error)]
#[error("the people republish queue failed: {0}")]
pub struct PeopleRepublishError(String);

impl PeopleRepublishError {
    /// Wraps a message, for the server's log.
    #[must_use]
    pub fn new(message: impl Into<String>) -> Self {
        Self(message.into())
    }
}

/// The queued stores in memory, by tenant then store, each with its reason and the time it was
/// queued.
type QueuedStores = BTreeMap<(TenantId, StoreId), (String, Timestamp)>;

/// The in-memory [`PeopleRepublishStore`], for tests and for code that runs without a database.
/// Bounded at [`IN_MEMORY_REPUBLISHES`] stores.
#[derive(Clone, Debug, Default)]
pub struct InMemoryPeopleRepublishes {
    rows: Arc<Mutex<QueuedStores>>,
}

impl InMemoryPeopleRepublishes {
    /// An empty queue.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }
}

impl PeopleRepublishStore for InMemoryPeopleRepublishes {
    async fn enqueue(&self, republish: &PeopleRepublish) -> Result<(), PeopleRepublishError> {
        let mut rows = self.rows.lock().unwrap_or_else(PoisonError::into_inner);
        let key = (republish.tenant_id, republish.store_id);
        if !rows.contains_key(&key) && rows.len() >= IN_MEMORY_REPUBLISHES {
            return Err(PeopleRepublishError::new(format!(
                "the in-memory people republish queue holds at most {IN_MEMORY_REPUBLISHES} stores"
            )));
        }
        rows.insert(key, (republish.reason.clone(), republish.enqueued_time));
        Ok(())
    }

    async fn pending(
        &self,
        after: Option<(TenantId, StoreId)>,
        limit: usize,
    ) -> Result<Vec<PeopleRepublish>, PeopleRepublishError> {
        let rows = self.rows.lock().unwrap_or_else(PoisonError::into_inner);
        Ok(rows
            .iter()
            .filter(|(key, _)| after.is_none_or(|after| **key > after))
            .take(limit)
            .map(
                |((tenant_id, store_id), (reason, enqueued_time))| PeopleRepublish {
                    tenant_id: *tenant_id,
                    store_id: *store_id,
                    reason: reason.clone(),
                    enqueued_time: *enqueued_time,
                },
            )
            .collect())
    }

    async fn clear(&self, republish: &PeopleRepublish) -> Result<bool, PeopleRepublishError> {
        let mut rows = self.rows.lock().unwrap_or_else(PoisonError::into_inner);
        let key = (republish.tenant_id, republish.store_id);
        let still = rows
            .get(&key)
            .is_some_and(|(_, enqueued_time)| *enqueued_time == republish.enqueued_time);
        if still {
            rows.remove(&key);
        }
        Ok(still)
    }
}

/// What publishing one queued store's people again did.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Republished {
    /// Its node was stale, and this version replaced it.
    Published(ConfigVersionId),
    /// Its node was already what its people compile to: no new version.
    Unchanged,
    /// Its configuration holds no `permissions` roster (a `staff` list) on any layer, so its
    /// people were never published: none was created.
    NeverPublished,
}

/// What one drain did, store by store, for the task-health record.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct DrainTally {
    /// Stores given a new version.
    pub published: usize,
    /// Stores whose node was already current.
    pub unchanged: usize,
    /// Stores with no roster, left without one.
    pub never_published: usize,
    /// Stores that could not be published, or whose row could not be cleared: still queued.
    pub failed: usize,
}

/// Drains the queue once: publishes every queued store's people again, one store at a time, and
/// clears each row it settled.
///
/// # Errors
///
/// [`PeopleRepublishError`] when the queue itself cannot be read: nothing after that read is
/// attempted, and the next drain starts again from the beginning. A store that cannot be published
/// is not an error here; it is counted, logged by its id and the refusal's HTTP status, and left
/// queued.
pub async fn drain<Q, P, Cfg, C>(
    queue: &Q,
    people: &P,
    config_trees: &Cfg,
    audit: &Arc<dyn AuditRecorder>,
    clock: &C,
) -> Result<DrainTally, PeopleRepublishError>
where
    Q: PeopleRepublishStore + Sync,
    P: EmployeeStore + RoleTemplateStore + AssignmentStore + Sync,
    Cfg: ConfigTreeStore + Sync,
    C: ClockSource + Sync,
{
    let mut tally = DrainTally::default();
    let mut after = None;
    loop {
        let batch = queue.pending(after, DRAIN_BATCH).await?;
        let full = batch.len() == DRAIN_BATCH;
        for queued in batch {
            after = Some((queued.tenant_id, queued.store_id));
            let republished = crate::http::republish_people(
                people,
                config_trees,
                clock,
                audit,
                queued.tenant_id,
                queued.store_id,
                &queued.reason,
            )
            .await;
            let republished = match republished {
                Ok(republished) => republished,
                Err(refusal) => {
                    // The status says whether the tree refused the node or the database failed:
                    // no personal data, and the store's id is the only identifier.
                    tracing::warn!(
                        store_id = %queued.store_id,
                        status = refusal.status().as_u16(),
                        "a store's people could not be published again; it stays queued for the next drain"
                    );
                    tally.failed += 1;
                    continue;
                }
            };
            if let Err(error) = queue.clear(&queued).await {
                tracing::error!(
                    %error,
                    store_id = %queued.store_id,
                    "a store's people were published again but its row could not be cleared"
                );
                tally.failed += 1;
                continue;
            }
            match republished {
                Republished::Published(_) => tally.published += 1,
                Republished::Unchanged => tally.unchanged += 1,
                Republished::NeverPublished => tally.never_published += 1,
            }
        }
        if !full {
            return Ok(tally);
        }
    }
}

/// Runs the people republisher until `shutdown` resolves: a drain at once, at start-up, then one
/// every `interval`, each recording its own health — the supervised-loop shape the scheduled-publish
/// activator and the alert evaluator have.
pub async fn run<Q, P, Cfg, Th, C>(
    queue: Q,
    people: P,
    config_trees: Cfg,
    audit: Arc<dyn AuditRecorder>,
    task_health: Th,
    clock: C,
    interval: Duration,
    shutdown: impl Future<Output = ()>,
) where
    Q: PeopleRepublishStore + Sync,
    P: EmployeeStore + RoleTemplateStore + AssignmentStore + Sync,
    Cfg: ConfigTreeStore + Sync,
    Th: TaskHealthStore + Sync,
    C: ClockSource + Sync,
{
    tokio::pin!(shutdown);
    loop {
        let detail = match drain(&queue, &people, &config_trees, &audit, &clock).await {
            Ok(tally) => {
                if tally != DrainTally::default() {
                    tracing::info!(
                        published = tally.published,
                        unchanged = tally.unchanged,
                        never_published = tally.never_published,
                        failed = tally.failed,
                        "the people republisher drained its queue"
                    );
                }
                tick_detail(
                    true,
                    interval.as_secs(),
                    serde_json::json!({
                        "published": tally.published,
                        "unchanged": tally.unchanged,
                        "never_published": tally.never_published,
                        "failed": tally.failed,
                    }),
                )
            }
            Err(error) => {
                tracing::error!(%error, "the people republisher could not read its queue; will retry");
                tick_detail(false, interval.as_secs(), serde_json::json!({}))
            }
        };
        if let Err(error) = task_health
            .record_tick(PEOPLE_REPUBLISHER, clock.now(), &detail)
            .await
        {
            tracing::warn!(%error, "recording people-republisher task health failed");
        }
        tokio::select! {
            biased;
            () = &mut shutdown => {
                tracing::info!("people republisher shutting down");
                return;
            }
            () = tokio::time::sleep(interval) => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use pos_proto::ids::{StoreId, TenantId};
    use pos_proto::time::Timestamp;
    use pos_proto::ulid::Ulid;

    use super::{
        IN_MEMORY_REPUBLISHES, InMemoryPeopleRepublishes, PeopleRepublish, PeopleRepublishStore,
    };

    fn queued(store: u128) -> PeopleRepublish {
        PeopleRepublish {
            tenant_id: TenantId::new(Ulid::from_u128(1)),
            store_id: StoreId::new(Ulid::from_u128(store)),
            reason: "0083_people_republishes".to_owned(),
            enqueued_time: Timestamp::EPOCH,
        }
    }

    #[tokio::test]
    async fn the_in_memory_queue_holds_a_bounded_number_of_stores() {
        let queue = InMemoryPeopleRepublishes::new();
        let bound = u128::try_from(IN_MEMORY_REPUBLISHES).unwrap_or(u128::MAX);
        for store in 0..bound {
            assert!(queue.enqueue(&queued(store)).await.is_ok());
        }
        assert!(
            queue.enqueue(&queued(bound)).await.is_err(),
            "one store past the bound is refused"
        );
        assert!(
            queue.enqueue(&queued(0)).await.is_ok(),
            "a store queued already can be queued again at the bound"
        );
    }
}
