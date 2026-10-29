// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! What a batch publish fans out: a node a group can receive, and how it is planned per store
//! (ADR-0122 §3).

use axum::response::Response;
use pos_cloud_api::response::api_error_with_details;
use pos_proto::ErrorStatus;
use pos_proto::ids::{StoreId, TenantId};

use crate::config_tree::ConfigTreeState;

// --- Batch publish to a store group (ADR-0122 §3–§7) -------------------------------------------

/// A boxed future, because a trait object cannot return `impl Future`.
///
/// The workspace carries no `async-trait` dependency and adding one is an ADR's worth of decision
/// (`docs/adr/0002-*`), so the two batch traits box by hand. One allocation per node per store,
/// against a database round-trip and a versioned config write — it does not register.
pub(in crate::http) type BoxFuture<'a, T> = core::pin::Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// One node kind a batch publish can deliver
/// ([ADR-0122](../../../docs/adr/0122-a-store-group-is-a-delivery-cohort.md) §4).
///
/// # Why a registry of trait objects and not thirteen routes
///
/// Each single-store publish lives behind its own `State` struct carrying its own authoring seam —
/// the tax table, the catalog, the roster. One batch route needs all of them. Making the route
/// generic over thirteen store types would give it thirteen type parameters and make adding a
/// fourteenth node a change to the route's signature, `main.rs`, and every test that builds one.
/// A `Vec<Arc<dyn BatchNode>>` erases them: `main.rs` builds the registry from the same store
/// handles the single-store routers already hold, the route knows only this trait, and a new node
/// kind is a new `impl` and one more line in the registry.
///
/// # What an implementation must not do
///
/// Compile its own document. Every implementation below delegates to the *same* `*_nodes` function
/// its single-store route calls, which is what makes ADR-0122 §3 true rather than aspirational: a
/// batch cannot validate more loosely than the route it fans out, because it is not a second copy
/// of the rule.
pub trait BatchNode: Send + Sync {
    /// The node's wire name — what a batch request names and what `config_batches.node` records.
    fn key(&self) -> &'static str;

    /// The audit action each member's publish records, identical to the single-store route's.
    fn audit_action(&self) -> &'static str;

    /// Config node keys a member's Store layer must already carry (ADR-0122 §7).
    ///
    /// Checked per store before anything is written; a member missing one is `skipped` with the
    /// missing key named, and the batch carries on. Empty for every node but `menu`.
    ///
    /// # Why only `menu` has any
    ///
    /// §7 seeds the table with exactly two rules — `menu` requires `tax` and `locale`, and every
    /// node requires the store to be active — and says the rest are added "as nodes acquire
    /// dependencies". A code audit of all sixteen publishes turned up four more *candidates*:
    /// `inventory` and `qr` arguably need `menu` (a recipe and a QR menu both key on item ids a
    /// store has no book for), `qr` and `tax` arguably need `locale` (no currency, nothing to
    /// price), and `campaigns` needs it only for the ones carrying a scheduled window or a money
    /// amount. None is adopted here, deliberately: each was one reader's reading, an added
    /// prerequisite makes a batch *skip* stores that would otherwise have been published to, and
    /// a wrong skip is as silent as the failure the rule exists to prevent. They are written down
    /// rather than dropped, so the next person to look has the list.
    fn prerequisites(&self) -> &'static [&'static str] {
        &[]
    }

    /// Reads the tenant-wide authoring model once, before any store is touched.
    ///
    /// A refusal here refuses the **whole batch** and writes nothing, because it means the
    /// instruction itself is unpublishable — an unknown capability flag, a menu that will not
    /// compile, an empty reason-code list — and would be the same answer at all 200 members.
    /// A per-store refusal is a different thing and belongs in [`BatchNodePlan::compile_for`].
    fn plan<'a>(
        &'a self,
        tenant_id: TenantId,
        arguments: &'a serde_json::Value,
    ) -> BoxFuture<'a, Result<Box<dyn BatchNodePlan + 'a>, Response>>;
}

/// The compiled half of a batch: what to write, once per member.
pub trait BatchNodePlan: Send + Sync {
    /// A compact, PII-free summary of what was compiled, for `config_batches` and the trail.
    fn summary(&self) -> serde_json::Value;

    /// The nodes to write at one member.
    ///
    /// Eleven of the thirteen ignore `store_id` entirely — see [`SameEverywhere`]. `permissions`
    /// and `floor` do not: their document *is* the store's own, so they compile here, once per
    /// member. Both are still batchable, because the instruction ("recompile each of these shops'
    /// own rosters") carries nothing store-specific even though every result does.
    fn compile_for(
        &self,
        store_id: StoreId,
    ) -> BoxFuture<'_, Result<Vec<(String, serde_json::Value)>, Response>>;

    /// Adds to `nodes` what a member's write sets beside them, composed on the member's tree as
    /// each attempt of the write reads it, and says what the member's report should say of it when
    /// the write leaves some of it behind.
    ///
    /// Nothing for every node but `menu`, which brings the store's `fees` along in the same version
    /// (ADR-0159). A release snapshots what [`Self::compile_for`] compiles and does not call this:
    /// what a store's tree holds is read when a write lands, not when it is scheduled.
    fn compose_on(
        &self,
        _store_id: StoreId,
        _tree: Option<&ConfigTreeState>,
        _nodes: &mut Vec<(String, serde_json::Value)>,
    ) -> Option<String> {
        None
    }
}

/// A plan whose document is byte-identical at every member — the shape ten of the thirteen have.
/// The menu's book is the same at every member too, beside the `fees` each member's write composes
/// ([`MenuBatchPlan`]).
pub(super) struct SameEverywhere {
    pub(super) nodes: Vec<(String, serde_json::Value)>,
    pub(super) summary: serde_json::Value,
}

impl SameEverywhere {
    /// Boxes this plan as the trait object [`BatchNode::plan`] returns.
    pub(super) fn boxed<'a>(self) -> Box<dyn BatchNodePlan + 'a> {
        Box::new(self)
    }
}

impl BatchNodePlan for SameEverywhere {
    fn summary(&self) -> serde_json::Value {
        self.summary.clone()
    }

    fn compile_for(
        &self,
        _store_id: StoreId,
    ) -> BoxFuture<'_, Result<Vec<(String, serde_json::Value)>, Response>> {
        Box::pin(core::future::ready(Ok(self.nodes.clone())))
    }
}

/// Deserialises a batch's free-form `arguments` into the shape one node takes.
///
/// The refusal names the node, because `arguments` is untyped on the wire and the operator's
/// mistake is nearly always "the right JSON for a different node kind".
#[expect(
    clippy::result_large_err,
    reason = "the refusal *is* an axum Response, the same shape every other route helper carries"
)]
pub(super) fn batch_arguments<T>(node: &str, arguments: &serde_json::Value) -> Result<T, Response>
where
    T: serde::de::DeserializeOwned,
{
    serde_json::from_value(arguments.clone()).map_err(|error| {
        api_error_with_details(
            ErrorStatus::InvalidArgument,
            format!("the arguments are not what the `{node}` node takes: {error}"),
            &[("arguments", "INVALID_VALUE")],
        )
    })
}
