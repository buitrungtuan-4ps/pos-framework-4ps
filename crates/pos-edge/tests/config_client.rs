// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The edge config-pull client swaps a rebuilt session into a *running* edge
//! ([ADR-0004](../../../docs/adr/0004-cloud-owned-configuration.md), ADR-0039).
//!
//! The transport is faked (no socket); the edge is a real [`Edge`] over the in-memory store. The test
//! proves the live rebuild: an edge that boots with an empty menu picks up a menu published from the
//! cloud on the next pull, without a restart — and a subsequent "up to date" pull is a no-op.

use std::sync::Arc;

use pos_edge::config_client::{ConfigClient, ConfigTransport, ConfigTransportError, SyncedConfig};
use pos_edge::{Edge, EdgeSession, InMemoryReceipts, StoreIdentity};
use pos_fakes::FakeStore;
use pos_fakes::executor::run_ready;
use pos_proto::SalesChannel;
use pos_proto::ids::{MenuItemId, StoreId, TaxClassId};
use pos_proto::menu::{MenuBook, MenuCatalog, MenuEntry};
use pos_proto::money::{CurrencyCode, Money};
use pos_proto::text::DisplayName;
use pos_proto::ulid::Ulid;

fn item() -> MenuItemId {
    MenuItemId::new(Ulid::from_u128(500))
}

fn edge() -> Arc<Edge<FakeStore>> {
    Arc::new(
        Edge::new(
            FakeStore::default(),
            StoreIdentity::for_store(StoreId::new(Ulid::from_u128(7))),
            EdgeSession::bootstrap(),
            Arc::new(InMemoryReceipts::new()),
        )
        .expect("seed the id generator"),
    )
}

/// A config transport that hands back one document the first time it is asked, then reports the store
/// is up to date — the store echoes its held version, and the cloud answers `None` once it matches.
#[derive(Clone)]
struct FakeConfigTransport {
    version: String,
    document: serde_json::Value,
}

impl ConfigTransport for FakeConfigTransport {
    async fn fetch(
        &self,
        held_version: Option<&str>,
    ) -> Result<Option<SyncedConfig>, ConfigTransportError> {
        if held_version == Some(self.version.as_str()) {
            return Ok(None);
        }
        Ok(Some(SyncedConfig {
            config_version_id: self.version.clone(),
            document: self.document.clone(),
        }))
    }
}

fn menu_document() -> serde_json::Value {
    let catalog = MenuCatalog::new().with(MenuEntry::new(
        item(),
        DisplayName::new("Margherita"),
        Money::new(CurrencyCode::VND, 99_000),
        TaxClassId::new(Ulid::from_u128(1)),
    ));
    // The bootstrap session's channel is DineIn.
    let book = MenuBook::new().with(SalesChannel::DineIn, catalog);
    serde_json::json!({ "menu": serde_json::to_value(&book).expect("serialize the book") })
}

#[test]
fn a_pulled_config_swaps_the_live_session_and_then_no_ops() {
    let edge = edge();
    assert!(
        edge.session().menu.get(item()).is_none(),
        "the edge boots with an empty menu"
    );

    let transport = FakeConfigTransport {
        version: "v1".to_owned(),
        document: menu_document(),
    };
    let client = ConfigClient::new(transport, edge.clone());

    // First pull applies the new menu to the running edge.
    let applied = run_ready(client.pump_once()).expect("pump");
    assert_eq!(applied.as_deref(), Some("v1"));
    assert!(
        edge.session().menu.get(item()).is_some(),
        "the published menu is live on the running edge — no restart"
    );

    // Second pull: the store now holds v1, so the cloud reports up to date and nothing changes.
    let again = run_ready(client.pump_once()).expect("second pump");
    assert_eq!(again, None, "an up-to-date store re-applies nothing");
    assert!(edge.session().menu.get(item()).is_some());
}
