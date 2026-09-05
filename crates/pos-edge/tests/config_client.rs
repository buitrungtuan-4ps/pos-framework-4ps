// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The edge config-pull client swaps a rebuilt session into a *running* edge
//! ([ADR-0004](../../../docs/adr/0004-cloud-owned-configuration.md), ADR-0039).
//!
//! The transport is faked (no socket); the edge is a real [`Edge`] over the in-memory store. The
//! tests prove two things. The live rebuild: an edge that boots with an empty menu picks up a menu
//! published from the cloud on the next pull, without a restart — and a subsequent "up to date" pull
//! is a no-op. And the offline restart (C1): the pulled document is written to the store's own
//! [`ConfigStore`](pos_ports::ConfigStore), so a box restarted with no cloud at all comes back on the
//! menu it last synced instead of on `EdgeSession::bootstrap`'s empty one.

use std::sync::Arc;

use pos_edge::config_client::{
    ConfigClient, ConfigTransport, ConfigTransportError, SyncedConfig, restore_session_from_store,
};
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

/// The store this edge is — the same id across a "restart", because [`ConfigStore`] is keyed by it.
fn store_id() -> StoreId {
    StoreId::new(Ulid::from_u128(7))
}

/// A published config version. A real ULID, because that is what the cloud mints and what the local
/// [`ConfigStore`](pos_ports::ConfigStore) stores a snapshot under.
fn version() -> String {
    Ulid::from_u128(42).to_string()
}

/// An edge over `store`, freshly booted: an empty menu, an empty roster, an empty floor.
fn edge_over(store: FakeStore) -> Arc<Edge<FakeStore>> {
    Arc::new(
        Edge::new(
            store,
            StoreIdentity::for_store(store_id()),
            EdgeSession::bootstrap(),
            Arc::new(InMemoryReceipts::new()),
        )
        .expect("seed the id generator"),
    )
}

fn edge() -> Arc<Edge<FakeStore>> {
    edge_over(FakeStore::default())
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
        version: version(),
        document: menu_document(),
    };
    let client = ConfigClient::new(transport, edge.clone(), None);

    // First pull applies the new menu to the running edge.
    let applied = run_ready(client.pump_once()).expect("pump");
    assert_eq!(applied.as_deref(), Some(version().as_str()));
    assert!(
        edge.session().menu.get(item()).is_some(),
        "the published menu is live on the running edge — no restart"
    );

    // Second pull: the store now holds v1, so the cloud reports up to date and nothing changes.
    let again = run_ready(client.pump_once()).expect("second pump");
    assert_eq!(again, None, "an up-to-date store re-applies nothing");
    assert!(edge.session().menu.get(item()).is_some());
}

#[test]
fn a_restarted_edge_comes_back_on_the_config_it_last_synced() {
    // One store, two `Edge`s over it: the second is the box after a restart. The cloud is present
    // for the first and gone for the second, which is the case that matters — an OTA install or a
    // power cut with the broadband down.
    let store = FakeStore::default();
    let before = edge_over(store.clone());
    let client = ConfigClient::new(
        FakeConfigTransport {
            version: version(),
            document: menu_document(),
        },
        before.clone(),
        None,
    );
    run_ready(client.pump_once()).expect("pump");
    assert!(before.session().menu.get(item()).is_some());

    // The restart. No transport is built at all: this box has no cloud to ask.
    let after = edge_over(store);
    assert!(
        after.session().menu.get(item()).is_none(),
        "a fresh edge boots on the empty bootstrap session"
    );

    let restored = run_ready(restore_session_from_store(&after)).expect("restore");
    assert_eq!(
        restored.as_deref(),
        Some(version().as_str()),
        "the restore reports the version now live, so the pull loop starts out holding it"
    );
    assert!(
        after.session().menu.get(item()).is_some(),
        "the restarted store can sell with no cloud reachable"
    );
}

#[test]
fn a_store_that_has_never_synced_restores_nothing_and_still_boots() {
    let edge = edge();
    let restored = run_ready(restore_session_from_store(&edge)).expect("restore");
    assert_eq!(restored, None, "a first boot has nothing to restore");
    assert!(edge.session().menu.get(item()).is_none());
}

#[test]
fn a_config_version_that_is_not_a_ulid_is_applied_but_not_stored() {
    // The local store keys a snapshot by `ConfigVersionId`, so a cloud that sent something else
    // cannot be stored. The counter must still get the menu — losing the sale is worse than losing
    // the offline restart — so this degrades to "re-pull after a restart" rather than refusing.
    let store = FakeStore::default();
    let before = edge_over(store.clone());
    let client = ConfigClient::new(
        FakeConfigTransport {
            version: "not-a-ulid".to_owned(),
            document: menu_document(),
        },
        before.clone(),
        None,
    );
    run_ready(client.pump_once()).expect("pump");
    assert!(
        before.session().menu.get(item()).is_some(),
        "the document is applied to the live session either way"
    );

    let after = edge_over(store);
    let restored = run_ready(restore_session_from_store(&after)).expect("restore");
    assert_eq!(restored, None, "nothing was stored, so nothing is restored");
}
