// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! A batch's request, report and per-store outcome, and the publish of one member.

use axum::response::Response;
use pos_proto::ErrorResponse;
use pos_proto::determinism::ClockSource;
use pos_proto::ids::{StoreId, TenantId};
use serde::{Deserialize, Serialize};

use super::batch_node::BatchNodePlan;
use crate::config_tree::{ConfigLevel, ConfigTreeStore};
use crate::http::{publish_config_nodes_with, read_store_node_as_run};
use crate::registry::EntityStatus;
use crate::store_groups::BatchOutcome;

/// A `POST /admin/store-groups/{group_id}/publish` body.
#[derive(Debug, Clone, Deserialize, utoipa::ToSchema)]
pub(super) struct PublishToStoreGroupRequest {
    pub(super) tenant_id: String,
    /// Which node kind to publish — one of the thirteen batchable ones (ADR-0122 §4).
    pub(super) node: String,
    /// That node's own arguments: the single-store body minus the `(tenant, store)`. Nodes
    /// compiled from tenant-wide authoring (`tax`, `campaigns`, `inventory`, `reason_codes`,
    /// `permissions`, `floor`) take none, and an object is accepted and ignored for them.
    #[serde(default)]
    pub(super) arguments: serde_json::Value,
}

/// What a batch did at one store.
#[derive(Debug, Clone, Serialize, utoipa::ToSchema)]
pub(super) struct BatchResultView {
    pub(super) store_id: String,
    /// `applied`, `skipped` or `failed`: exactly the three of ADR-0122 §6.
    pub(super) outcome: BatchOutcome,
    /// The refusal's own message, for `skipped` and `failed`. For `applied`, absent unless the
    /// write left something behind: a menu whose store's fees did not go with it says so, with the
    /// fee publish's outcome and reason tokens.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schema(nullable = false)]
    pub(super) detail: Option<String>,
    /// The config version the publish produced, for `applied`.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schema(nullable = false)]
    pub(super) config_version_id: Option<String>,
    pub(super) at_ms: i64,
}

/// A batch and what it did at every member — the response body, and the read routes' shape.
#[derive(Debug, Clone, Serialize, utoipa::ToSchema)]
pub(super) struct BatchReport {
    pub(super) batch_id: String,
    pub(super) group_id: String,
    pub(super) node: String,
    pub(super) arguments: serde_json::Value,
    pub(super) actor_email: String,
    pub(super) started_at_ms: i64,
    /// Absent for a batch that died mid-fan-out — exactly the row an operator goes looking for.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schema(nullable = false)]
    pub(super) finished_at_ms: Option<i64>,
    pub(super) applied: usize,
    pub(super) skipped: usize,
    pub(super) failed: usize,
    pub(super) results: Vec<BatchResultView>,
}

/// The most bytes a refusal body may contribute to a batch result's `detail`.
///
/// A refusal is one sentence and a `details` array; this is a bound on a value the batch stores
/// once per member, not a wire limit.
const MAX_REFUSAL_DETAIL_BYTES: usize = 4096;

/// Renders a route's own refusal into the one line a batch result records.
///
/// The batch does not invent a message: it collects the AIP-193 envelope the single-store route
/// would have returned and stores its `message`. That is what ADR-0122 §6's "the refusal's own
/// message" means, and it is a correctness property rather than a convenience — the operator
/// reading a batch report and the operator who published to one store are told the same thing.
/// A body that will not parse degrades to the status code, which is still more than "failed".
async fn refusal_detail(response: Response) -> String {
    let status = response.status();
    let Ok(bytes) = axum::body::to_bytes(response.into_body(), MAX_REFUSAL_DETAIL_BYTES).await
    else {
        return format!("the publish was refused with HTTP {status}");
    };
    serde_json::from_slice::<ErrorResponse>(&bytes).map_or_else(
        |_| format!("the publish was refused with HTTP {status}"),
        |envelope| envelope.error.message,
    )
}

/// One member's outcome, before it is written to `config_batch_results`.
pub(super) struct MemberOutcome {
    pub(super) outcome: BatchOutcome,
    pub(super) detail: Option<String>,
    pub(super) version_id: Option<String>,
}

impl MemberOutcome {
    /// The store is deliberately protected, or a prerequisite is missing: `skipped`, with why.
    fn skipped(detail: impl Into<String>) -> Self {
        Self {
            outcome: BatchOutcome::Skipped,
            detail: Some(detail.into()),
            version_id: None,
        }
    }

    /// The publish was attempted and refused: `failed`, carrying the refusal's own message.
    fn failed(detail: impl Into<String>) -> Self {
        Self {
            outcome: BatchOutcome::Failed,
            detail: Some(detail.into()),
            version_id: None,
        }
    }
}

/// Publishes one node to one member, deciding between the three outcomes of ADR-0122 §6.
///
/// The order matters and is the record's: the **protective** checks run first and produce
/// `skipped`, so a store that was never going to be written is never filed beside one that broke.
/// Only once a member has passed them is anything compiled or written, and from there a refusal is
/// `failed`.
#[expect(
    clippy::result_large_err,
    reason = "the composing closure's Err is an axum Response by design — the shape every node \
              publish's closure carries — and this closure never takes the Err branch"
)]
pub(super) async fn publish_batch_member<Cfg, C>(
    config_trees: &Cfg,
    clock: &C,
    plan: &dyn BatchNodePlan,
    prerequisites: &[&str],
    tenant_id: TenantId,
    store_id: StoreId,
    store_status: Option<EntityStatus>,
) -> MemberOutcome
where
    Cfg: ConfigTreeStore,
    C: ClockSource,
{
    // A membership row can outlive the store it names — there is no foreign key, deliberately
    // (migration 0061). Both of these are the cohort being one store short of what the operator
    // believes it is, which is worth saying every time rather than passing over in silence.
    match store_status {
        None => {
            return MemberOutcome::skipped("this store is not in the tenant's registry");
        }
        Some(EntityStatus::Archived) => {
            return MemberOutcome::skipped("this store is archived");
        }
        Some(_) => {}
    }

    for key in prerequisites {
        match read_store_node_as_run(config_trees, tenant_id, store_id, key).await {
            Ok(serde_json::Value::Null) => {
                return MemberOutcome::skipped(format!(
                    "this store has no `{key}` node yet; publish one to it before this"
                ));
            }
            Ok(_) => {}
            Err(refusal) => return MemberOutcome::failed(refusal_detail(refusal).await),
        }
    }

    let nodes = match plan.compile_for(store_id).await {
        Ok(nodes) => nodes,
        Err(refusal) => return MemberOutcome::failed(refusal_detail(refusal).await),
    };
    // What rides along — a menu's `fees` — is composed on the tree each attempt of the write reads.
    let mut note = None;
    match publish_config_nodes_with(
        config_trees,
        clock,
        tenant_id,
        store_id,
        ConfigLevel::Store,
        |tree| {
            let mut writing = nodes.clone();
            note = plan.compose_on(store_id, tree, &mut writing);
            Ok(writing)
        },
    )
    .await
    {
        Ok(version_id) => MemberOutcome {
            outcome: BatchOutcome::Applied,
            detail: note,
            version_id: Some(version_id.to_string()),
        },
        Err(refusal) => MemberOutcome::failed(refusal_detail(refusal).await),
    }
}
