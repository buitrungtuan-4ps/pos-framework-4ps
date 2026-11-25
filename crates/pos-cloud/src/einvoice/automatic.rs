// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The cloud allocates a store's e-invoice symbol by itself where `einvoice.symbol_allocation` is
//! `AUTOMATIC` ([ADR-0240](../../../../docs/adr/0240-the-cloud-allocates-each-issuing-points-invoice-symbol.md)
//! decision 4): when a box is activated for a store with no symbol, and with each lease bump.
//!
//! An allocation that fails never fails the activation or the bump. It is logged, and the store
//! refuses to settle until a symbol is published, which is the safe failure.

use std::sync::Arc;

use pos_cloud_api::auth::admin::system_actor;
use pos_cloud_api::console::audit_action;
use pos_cloud_devices::activation::{
    ActivationCodeStore, ActivationStoreError, DeviceCredential, IssuedCode,
};
use pos_proto::determinism::ClockSource;
use pos_proto::einvoice::{EInvoiceSymbolAllocation, PublishedEInvoice};
use pos_proto::ids::{DeviceId, StoreId, TenantId};

use super::symbols::{SymbolLedger, SymbolRecord, allocate};
use crate::audit::AuditRecorder;
use crate::config_tree::{ConfigLevel, ConfigTreeStore};
use crate::http::config_nodes::{symbol_node, utc_year};

/// What asked for the allocation, as the audit trail records it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Trigger {
    /// A box was activated for a store with no symbol.
    Activation,
    /// The store's lease was bumped for a replacement box.
    LeaseBump,
}

impl Trigger {
    /// The token the audit detail carries.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Activation => "activation",
            Self::LeaseBump => "lease_bump",
        }
    }
}

/// Allocates and records the store's next symbol when its effective `einvoice` node says
/// `AUTOMATIC` and, on activation, the store has no symbol yet. `None` when it should not, or when
/// it could not; a failure is logged.
pub async fn allocate_if_automatic<Cfg: ConfigTreeStore>(
    trees: &Cfg,
    ledger: &dyn SymbolLedger,
    tenant: TenantId,
    store: StoreId,
    lease_generation: Option<i64>,
    now: pos_proto::time::Timestamp,
    trigger: Trigger,
) -> Option<SymbolRecord> {
    let tree = match trees.load(tenant, store).await {
        Ok(tree) => tree?,
        Err(error) => {
            tracing::warn!(%error, store = %store, "could not read the store's configuration; no e-invoice symbol was allocated");
            return None;
        }
    };
    let effective = tree.record.effective();
    let node: PublishedEInvoice = effective
        .get(PublishedEInvoice::NODE)
        .and_then(|node| serde_json::from_value(node.clone()).ok())
        .unwrap_or_default();
    if node.symbol_allocation() != EInvoiceSymbolAllocation::Automatic {
        return None;
    }
    if trigger == Trigger::Activation && node.symbol().is_some() {
        return None;
    }
    let countries = crate::countries::registry();
    let country = crate::http::locale_node::published_country(&tree.record)
        .and_then(|code| countries.get(code));
    match allocate(
        ledger,
        country,
        tenant,
        Some(store),
        lease_generation,
        utc_year(now),
        now,
    )
    .await
    {
        Ok(record) => Some(record),
        Err(error) => {
            tracing::warn!(?error, store = %store, trigger = trigger.as_str(), "no e-invoice symbol was allocated; the store refuses to settle until one is published");
            None
        }
    }
}

/// Publishes `record`'s symbol on the store's `einvoice` node and audits it as the system.
#[expect(
    clippy::result_large_err,
    reason = "the composing closure's Err is an axum Response, the shape publish_config_nodes_with takes"
)]
pub async fn publish_allocated<Cfg, C>(
    trees: &Cfg,
    clock: &C,
    audit: &Arc<dyn AuditRecorder>,
    tenant: TenantId,
    store: StoreId,
    record: &SymbolRecord,
    trigger: Trigger,
) where
    Cfg: ConfigTreeStore,
    C: ClockSource,
{
    let published = pos_cloud_config::publish::publish_config_nodes_with(
        trees,
        clock,
        tenant,
        store,
        ConfigLevel::Store,
        |tree| Ok(vec![symbol_node(tree, &record.symbol)]),
    )
    .await;
    if published.is_err() {
        tracing::warn!(store = %store, "an allocated e-invoice symbol could not be published; publish it from the console");
        return;
    }
    audit_action(
        audit,
        clock,
        &system_actor(),
        Some(tenant),
        "config.einvoice_symbol.allocate",
        "store",
        &store.to_string(),
        None,
        Some(serde_json::json!({
            "symbol": record.symbol,
            "source": record.source.as_str(),
            "trigger": trigger.as_str(),
        })),
    )
    .await;
}

/// The activation store, allocating a symbol once a code is redeemed for a store that has none
/// (ADR-0240 decision 4). Every other call passes through unchanged.
#[derive(Clone)]
#[expect(
    missing_debug_implementations,
    reason = "it holds trait objects that have no Debug, and nothing logs it"
)]
pub struct AllocatingActivations<X, Cfg, C> {
    inner: X,
    trees: Cfg,
    clock: C,
    audit: Arc<dyn AuditRecorder>,
    ledger: Arc<dyn SymbolLedger>,
}

impl<X, Cfg, C> AllocatingActivations<X, Cfg, C> {
    /// Wraps `inner`.
    pub fn new(
        inner: X,
        trees: Cfg,
        clock: C,
        audit: Arc<dyn AuditRecorder>,
        ledger: Arc<dyn SymbolLedger>,
    ) -> Self {
        Self {
            inner,
            trees,
            clock,
            audit,
            ledger,
        }
    }
}

impl<X, Cfg, C> ActivationCodeStore for AllocatingActivations<X, Cfg, C>
where
    X: ActivationCodeStore + Sync,
    Cfg: ConfigTreeStore + Sync,
    C: ClockSource + Sync,
{
    async fn issue(
        &self,
        code_hash: [u8; 32],
        tenant_id: TenantId,
        store_id: StoreId,
        device_id: DeviceId,
    ) -> Result<(), ActivationStoreError> {
        self.inner
            .issue(code_hash, tenant_id, store_id, device_id)
            .await
    }

    async fn lookup(
        &self,
        code_hash: [u8; 32],
    ) -> Result<Option<IssuedCode>, ActivationStoreError> {
        self.inner.lookup(code_hash).await
    }

    async fn consume_and_provision(
        &self,
        code_hash: [u8; 32],
        credential: &DeviceCredential,
    ) -> Result<bool, ActivationStoreError> {
        let slot = self.inner.lookup(code_hash).await?;
        let consumed = self
            .inner
            .consume_and_provision(code_hash, credential)
            .await?;
        if let (true, Some(slot)) = (consumed, slot) {
            let now = self.clock.now();
            let allocated = allocate_if_automatic(
                &self.trees,
                self.ledger.as_ref(),
                slot.tenant_id,
                slot.store_id,
                None,
                now,
                Trigger::Activation,
            )
            .await;
            if let Some(record) = allocated {
                publish_allocated(
                    &self.trees,
                    &self.clock,
                    &self.audit,
                    slot.tenant_id,
                    slot.store_id,
                    &record,
                    Trigger::Activation,
                )
                .await;
            }
        }
        Ok(consumed)
    }

    async fn revoke_slot(
        &self,
        tenant_id: TenantId,
        store_id: StoreId,
        device_id: DeviceId,
    ) -> Result<u64, ActivationStoreError> {
        self.inner.revoke_slot(tenant_id, store_id, device_id).await
    }
}
