// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! Erasing a subject that a store holds
//! ([ADR-0206](../../../docs/adr/0206-a-personal-custom-value-stays-on-its-store-and-expires-on-its-own-clock.md)
//! decision 4).
//!
//! A personal custom value stays on the store that collected it. When the owner erases a subject
//! the cloud does not hold, the erase route finds the stores whose events name it
//! ([`SubjectHolders`], from the `custom_subjects` table the projector folds) and adds the id to
//! each one's `erasures` node ([`append_erasure`]). The store masks the subject and confirms with
//! `data.subject.masked`; the projector folds that and takes the id off the node
//! ([`prune_confirmed`]). Until then the console shows the erasure as pending.
//!
//! Only ids travel. The cloud never holds, reads or logs a personal value here.

use core::future::Future;

use axum::response::Response;
use pos_cloud_api::response::api_error;
use pos_cloud_reports::custom_data::{CustomValueChange, CustomValueStore};
use pos_ports::PortError;
use pos_proto::ErrorStatus;
use pos_proto::determinism::ClockSource;
use pos_proto::erasures::{Erasures, MAX_ERASURES};
use pos_proto::ids::{EventId, StoreId, SubjectId, TenantId};
use pos_proto::time::Timestamp;

use crate::config_tree::{ConfigLevel, ConfigTreeState, ConfigTreeStore};
use crate::dashboard::projector::ConfirmedErasures;
use pos_cloud_config::publish::publish_config_nodes_with;

/// One store whose events name a subject.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SubjectHolder {
    /// The store.
    pub store_id: StoreId,
    /// When the store confirmed masking the subject, or `None` while it has not.
    pub masked_time: Option<Timestamp>,
}

/// Which stores hold a subject, read from the `custom_subjects` table (store-postgres migration
/// 0093). One subject at a time, never a list.
pub trait SubjectHolders {
    /// The stores whose events name `subject_id`, at most 64.
    fn holders(
        &self,
        tenant: TenantId,
        subject_id: SubjectId,
    ) -> impl Future<Output = Result<Vec<SubjectHolder>, PortError>> + Send;

    /// Which of `subject_ids` the store has confirmed masking.
    fn masked_among(
        &self,
        tenant: TenantId,
        store_id: StoreId,
        subject_ids: &[SubjectId],
    ) -> impl Future<Output = Result<Vec<SubjectId>, PortError>> + Send;
}

impl SubjectHolders for store_postgres::PostgresCustomValues {
    async fn holders(
        &self,
        tenant: TenantId,
        subject_id: SubjectId,
    ) -> Result<Vec<SubjectHolder>, PortError> {
        let rows = self
            .subject_holders(tenant, &subject_id.to_string())
            .await?;
        Ok(rows
            .into_iter()
            .filter_map(|(store_id, masked_ms)| {
                Some(SubjectHolder {
                    store_id: store_id.parse().ok()?,
                    masked_time: masked_ms
                        .and_then(|ms| Timestamp::from_milliseconds_since_epoch(ms).ok()),
                })
            })
            .collect())
    }

    async fn masked_among(
        &self,
        tenant: TenantId,
        store_id: StoreId,
        subject_ids: &[SubjectId],
    ) -> Result<Vec<SubjectId>, PortError> {
        let ids: Vec<String> = subject_ids.iter().map(ToString::to_string).collect();
        let masked =
            store_postgres::PostgresCustomValues::masked_among(self, tenant, store_id, &ids)
                .await?;
        Ok(masked.iter().filter_map(|id| id.parse().ok()).collect())
    }
}

/// The ids the store's `erasures` node names now, read from the tree's layers composed. An
/// unreadable node reads as naming none.
#[must_use]
pub fn requested(state: Option<&ConfigTreeState>) -> Vec<SubjectId> {
    state
        .map(ConfigTreeState::effective)
        .and_then(|document| Erasures::read(&document).ok().flatten())
        .map(|node| node.subject_ids)
        .unwrap_or_default()
}

/// The `erasures` node as the store runs it, with `subject_id` added.
///
/// Called on every attempt of the conditional-write retry, as the device deny-list's append is, so
/// a concurrent erasure is never lost. Idempotent: an id already there publishes the same list.
/// Refuses, naming the cap, an erasure that would take the node over [`MAX_ERASURES`] (ADR-0206
/// decision 4).
///
/// # Errors
///
/// The `400` the route returns when the node is full.
#[expect(
    clippy::result_large_err,
    reason = "the Err is an axum Response by design — it is the refusal the route returns"
)]
pub fn append_erasure(
    before: Option<&ConfigTreeState>,
    subject_id: SubjectId,
) -> Result<Vec<(String, serde_json::Value)>, Response> {
    let mut ids = requested(before);
    if !ids.contains(&subject_id) {
        if ids.len() >= MAX_ERASURES {
            return Err(api_error(
                ErrorStatus::InvalidArgument,
                format!(
                    "this store's erasures node already names {MAX_ERASURES} subjects, which is its \
                     cap; try again once the store has confirmed the ones it holds"
                ),
            ));
        }
        ids.push(subject_id);
    }
    Ok(vec![node(ids)])
}

/// The node naming `ids`.
fn node(subject_ids: Vec<SubjectId>) -> (String, serde_json::Value) {
    (
        Erasures::NODE.to_owned(),
        serde_json::to_value(Erasures { subject_ids })
            .unwrap_or_else(|_| serde_json::json!({ "subject_ids": [] })),
    )
}

/// Takes off the store's `erasures` node every id the store has confirmed masking, and returns how
/// many it took off. Reads the node, asks [`SubjectHolders::masked_among`] which ids are confirmed,
/// and publishes the rest, filtering again inside the retry so a concurrent erasure is kept.
///
/// # Errors
///
/// A description of what failed, for the projector's log. The ids stay on the node and are taken
/// off on the store's next confirmation.
#[expect(
    clippy::result_large_err,
    reason = "the composing closure's Err is an axum Response by design, as every node publish's is"
)]
pub async fn prune_confirmed<Cfg, H, C>(
    config_trees: &Cfg,
    holders: &H,
    clock: &C,
    tenant: TenantId,
    store_id: StoreId,
) -> Result<usize, String>
where
    Cfg: ConfigTreeStore,
    H: SubjectHolders,
    C: ClockSource,
{
    let loaded = config_trees
        .load(tenant, store_id)
        .await
        .map_err(|error| error.to_string())?;
    let named = requested(loaded.as_ref().map(|versioned| &versioned.record));
    if named.is_empty() {
        return Ok(0);
    }
    let confirmed = holders
        .masked_among(tenant, store_id, &named)
        .await
        .map_err(|error| error.to_string())?;
    if confirmed.is_empty() {
        return Ok(0);
    }
    publish_config_nodes_with(
        config_trees,
        clock,
        tenant,
        store_id,
        ConfigLevel::Store,
        |before| {
            let mut ids = requested(before);
            ids.retain(|id| !confirmed.contains(id));
            Ok(vec![node(ids)])
        },
    )
    .await
    .map_err(|refusal| format!("the publish was refused with {}", refusal.status()))?;
    Ok(confirmed.len())
}

/// The projector's custom-value store, which also takes confirmed ids off a store's `erasures`
/// node: the value table, the config tree and the clock a publish stamps.
#[derive(Clone, Debug)]
pub struct CustomProjection<V, Cfg, C> {
    values: V,
    config_trees: Cfg,
    clock: C,
}

impl<V, Cfg, C> CustomProjection<V, Cfg, C> {
    /// The projector's store over `values`, publishing to `config_trees`.
    #[must_use]
    pub const fn new(values: V, config_trees: Cfg, clock: C) -> Self {
        Self {
            values,
            config_trees,
            clock,
        }
    }
}

impl<V, Cfg, C> CustomValueStore for CustomProjection<V, Cfg, C>
where
    V: CustomValueStore + Sync,
    Cfg: Sync,
    C: Sync,
{
    fn cursor(
        &self,
        tenant: TenantId,
        store_id: StoreId,
    ) -> impl Future<Output = Result<Option<EventId>, PortError>> + Send {
        self.values.cursor(tenant, store_id)
    }

    fn apply(
        &self,
        tenant: TenantId,
        store_id: StoreId,
        changes: &[CustomValueChange],
        cursor: EventId,
    ) -> impl Future<Output = Result<(), PortError>> + Send {
        self.values.apply(tenant, store_id, changes, cursor)
    }
}

impl<V, Cfg, C> ConfirmedErasures for CustomProjection<V, Cfg, C>
where
    V: SubjectHolders + Sync,
    Cfg: ConfigTreeStore + Sync,
    C: ClockSource + Sync,
{
    async fn prune(&self, tenant: TenantId, store_id: StoreId) {
        match prune_confirmed(
            &self.config_trees,
            &self.values,
            &self.clock,
            tenant,
            store_id,
        )
        .await
        {
            Ok(0) => {}
            Ok(pruned) => tracing::info!(
                %tenant, %store_id, pruned,
                "took the subjects a store confirmed masked off its erasures node"
            ),
            Err(error) => tracing::warn!(
                %tenant, %store_id, %error,
                "could not take confirmed subjects off a store's erasures node; they stay pending \
                 until its next confirmation"
            ),
        }
    }
}

#[cfg(test)]
mod tests;
