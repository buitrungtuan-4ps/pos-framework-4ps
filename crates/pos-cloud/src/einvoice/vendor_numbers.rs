// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! A vendor numbers the invoices a store issued "number pending"
//! ([ADR-0251](../../../../docs/adr/0251-a-vendor-numbered-invoice-waits-for-its-number-and-the-store-learns-it.md)
//! decisions 3 and 4).
//!
//! The feed records such an invoice under a placeholder that names its event, never a number, and
//! submits it with no number, so the provider takes its own. The provider's number then replaces the
//! placeholder, and the cloud writes it on the store's own `einvoice` node, as it writes the symbol,
//! so the store learns it at its next config pull. No port changes: `Fiscalization::issue` with no
//! `issued_number` already lets a provider number an invoice. Every statement of law behind it is to
//! be confirmed by legal or the accountant.

use std::sync::{Arc, Mutex};

use crate::config_tree::{ConfigLevel, ConfigTreeState, ConfigTreeStore};
use pos_ports::PortError;
use pos_ports::fiscalization::{Fiscalization, InvoiceRequest, IssuedInvoice};
use pos_proto::einvoice::{PublishedEInvoice, VENDOR_NUMBERS_MAX, VendorNumber};
use pos_proto::ids::{EventId, StoreId, TenantId};
use pos_proto::{ClockSource, ErrorStatus};

use super::feed::FeedFuture;

/// What a pending invoice's number reads as until the vendor gives one: the prefix, then the
/// event's id, so each is unique in the tenant's numbers and none parses as a symbol's.
pub const PENDING_PREFIX: &str = "PENDING/";

/// How many numbers the cloud asks a provider for when a pending invoice finds none left: the
/// provider's own block, which a vendor that numbers by itself never needs.
pub const VENDOR_BLOCK: u32 = 100;

/// The placeholder number of the pending invoice recorded by `event_id`.
#[must_use]
pub fn pending_number(event_id: EventId) -> String {
    format!("{PENDING_PREFIX}{event_id}")
}

/// Whether a recorded number is still a placeholder.
#[must_use]
pub fn is_pending(number: &str) -> bool {
    number.starts_with(PENDING_PREFIX)
}

/// Asks `provider` to number a pending invoice. A provider that numbers from blocks and has none
/// left is given one more block, once, and asked again.
///
/// # Errors
///
/// The provider's refusal or failure, which the submitter handles as for any invoice.
pub async fn number<F: Fiscalization>(
    provider: &F,
    request: &InvoiceRequest,
) -> Result<IssuedInvoice, PortError> {
    match provider.issue(request).await {
        Err(error) if error.status() == ErrorStatus::ResourceExhausted => {
            let block =
                core::num::NonZeroU32::new(VENDOR_BLOCK).unwrap_or(core::num::NonZeroU32::MIN);
            provider.allocate_range(request.store_id, block).await?;
            provider.issue(request).await
        }
        answered => answered,
    }
}

/// Where the cloud writes a vendor's number for the store to learn.
pub trait VendorNumbers: Send + Sync {
    /// Writes `entry` on the store's own `einvoice` node and publishes it.
    fn write(&self, tenant: TenantId, store: StoreId, entry: VendorNumber) -> FeedFuture<'_, ()>;
}

/// [`VendorNumbers`] over the configuration trees: the Store layer's `einvoice` node, its other
/// fields kept.
#[derive(Debug, Clone)]
pub struct Published<Cfg, C> {
    /// The trees.
    pub trees: Cfg,
    /// The clock a publish stamps.
    pub clock: C,
}

impl<Cfg, C> VendorNumbers for Published<Cfg, C>
where
    Cfg: ConfigTreeStore + Send + Sync,
    C: ClockSource + Send + Sync,
{
    #[expect(
        clippy::result_large_err,
        reason = "the composing closure's Err is an axum Response, the shape publish_config_nodes_with takes"
    )]
    fn write(&self, tenant: TenantId, store: StoreId, entry: VendorNumber) -> FeedFuture<'_, ()> {
        Box::pin(async move {
            let entry = serde_json::to_value(&entry).map_err(|error| error.to_string())?;
            pos_cloud_config::publish::publish_config_nodes_with(
                &self.trees,
                &self.clock,
                tenant,
                store,
                ConfigLevel::Store,
                |tree| Ok(vec![numbers_node(tree, &entry)]),
            )
            .await
            .map(|_version| ())
            .map_err(|refused| {
                format!(
                    "the vendor's number was not published: {}",
                    refused.status()
                )
            })
        })
    }
}

/// The store's `einvoice` node with `entry` in its `vendor_numbers`: the node's other fields kept,
/// an earlier entry for the same bill replaced, and the newest [`VENDOR_NUMBERS_MAX`] kept.
pub(crate) fn numbers_node(
    tree: Option<&ConfigTreeState>,
    entry: &serde_json::Value,
) -> (String, serde_json::Value) {
    let mut node = tree
        .and_then(|tree| tree.layer(ConfigLevel::Store).get(PublishedEInvoice::NODE))
        .filter(|node| node.is_object())
        .cloned()
        .unwrap_or_else(|| serde_json::json!({}));
    let mut numbers: Vec<serde_json::Value> = node
        .get("vendor_numbers")
        .and_then(serde_json::Value::as_array)
        .cloned()
        .unwrap_or_default();
    numbers.retain(|held| held.get("bill_id") != entry.get("bill_id"));
    numbers.push(entry.clone());
    let excess = numbers.len().saturating_sub(VENDOR_NUMBERS_MAX);
    numbers.drain(..excess);
    node["vendor_numbers"] = serde_json::Value::Array(numbers);
    (PublishedEInvoice::NODE.to_owned(), node)
}

/// [`VendorNumbers`] that keeps what it was given, for tests and for a cloud with no trees.
#[derive(Debug, Default, Clone)]
pub struct Recorded(pub Arc<Mutex<Vec<(TenantId, StoreId, VendorNumber)>>>);

impl VendorNumbers for Recorded {
    fn write(&self, tenant: TenantId, store: StoreId, entry: VendorNumber) -> FeedFuture<'_, ()> {
        let done = self
            .0
            .lock()
            .map(|mut written| {
                if written.len() < VENDOR_NUMBERS_MAX {
                    written.push((tenant, store, entry));
                }
            })
            .map_err(|_| "poisoned".to_owned());
        Box::pin(async move { done })
    }
}

#[cfg(test)]
mod tests {
    use super::{is_pending, numbers_node, pending_number};
    use pos_proto::ids::EventId;
    use pos_proto::ulid::Ulid;

    #[test]
    fn a_placeholder_is_never_a_number() {
        let placeholder = pending_number(EventId::new(Ulid::from_u128(7)));
        assert!(is_pending(&placeholder));
        assert!(!is_pending("1C26MAA/00000007"));
    }

    #[test]
    fn a_number_replaces_the_bills_entry_keeps_the_node_and_the_newest_two_hundred() {
        let numbers: Vec<serde_json::Value> = (0..205)
            .map(|bill| serde_json::json!({ "bill_id": format!("B{bill}"), "invoice_number": format!("V/{bill}") }))
            .collect();
        let tree = crate::config_tree::ConfigTreeState {
            layers: [
                serde_json::json!({}),
                serde_json::json!({}),
                serde_json::json!({ "einvoice": { "symbol": "1C26MAA", "vendor_numbers": numbers } }),
                serde_json::json!({}),
            ],
            history: Vec::new(),
            k: 3,
        };
        let entry = serde_json::json!({ "bill_id": "B204", "invoice_number": "V/204-again" });
        let (key, written) = numbers_node(Some(&tree), &entry);
        assert_eq!(key, "einvoice");
        assert_eq!(written["symbol"], "1C26MAA");
        let list = written["vendor_numbers"].as_array().expect("a list");
        assert_eq!(list.len(), 200);
        assert_eq!(list[0]["bill_id"], "B5");
        assert_eq!(list[199], entry);
        assert_eq!(
            list.iter().filter(|held| held["bill_id"] == "B204").count(),
            1
        );
    }
}
