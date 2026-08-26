// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The edge config-pull loop and live session rebuild
//! ([ADR-0004](../../../docs/adr/0004-cloud-owned-configuration.md),
//! [ADR-0033](../../../docs/adr/0033-config-tree.md), [ADR-0039](../../../docs/adr/0039-config-delivery.md)).
//!
//! Configuration is cloud-owned (ADR-0004): the store never edits its menu, tax table, or capability
//! flags — it *receives* them. This is the receiver. A background loop pulls the store's effective
//! config document from the cloud, rebuilds an [`EdgeSession`] from it, and swaps that session into
//! the running [`Edge`] with [`Edge::apply_session`] — so a menu published from the dashboard reaches
//! the counter without a restart, and a command already in flight keeps the session it started with.
//!
//! The document→session step ([`session_from_config`]) is pure and forgiving: it reads the compiled
//! `menu` node the catalog publish writes (ADR-0066), and an absent or unparseable node leaves that
//! part of the session unchanged rather than blanking the menu — a bad publish must never take a
//! trading store's price book away. The HTTP is a seam ([`ConfigTransport`]), so the loop is tested
//! with no socket.
//!
//! Scope: this rebuilds and hot-swaps the live session from the pulled document. Persisting the pulled
//! document to the edge's local [`ConfigStore`](pos_ports::config_store::ConfigStore) (so a restart
//! keeps the last-synced menu without a round-trip), and interpreting the delta form of a config
//! update, are the store-sqlite integration that layers on this seam.

use core::future::Future;
use core::time::Duration;
use std::sync::{Arc, Mutex};

use pos_proto::menu::MenuBook;

use crate::app::{Edge, EdgeSession};

/// How long the loop waits after a transport error before retrying — the store trades locally with
/// its last-known-good session while the cloud link is down, so this is a background reconnect.
const RETRY_BACKOFF: Duration = Duration::from_secs(5);

/// Rebuilds an [`EdgeSession`] from a synced config document, on top of a base session.
///
/// Reads the compiled `menu` node (a [`MenuBook`], ADR-0066) and installs the catalog for the
/// session's channel. Any node that is absent or does not parse is left as the base has it — a bad or
/// partial publish never blanks a field, it just does not change it.
#[must_use]
pub fn session_from_config(base: &EdgeSession, document: &serde_json::Value) -> EdgeSession {
    let channel = base.sales_channel;
    let mut session = base.clone();
    // Parse the node via its JSON text, not `from_value`: some wire types (e.g. `CurrencyCode`)
    // deserialize from a *borrowed* `&str`, which `from_str` supports but `from_value` cannot.
    if let Some(book) = document
        .get("menu")
        .and_then(|value| serde_json::to_string(value).ok())
        .and_then(|text| serde_json::from_str::<MenuBook>(&text).ok())
    {
        session.menu = book.catalog_for(channel).clone();
    }
    session
}

/// A store's effective config as pulled from the cloud: its version and the document itself.
#[derive(Debug, Clone)]
pub struct SyncedConfig {
    /// The config version this document is, echoed back on the next pull so the cloud can answer
    /// "up to date" without resending.
    pub config_version_id: String,
    /// The effective config document (the merged Tenant→Brand→Store→Device tree).
    pub document: serde_json::Value,
}

/// A failure of the config transport itself — the cloud is unreachable or answered unparseably.
#[derive(Debug, thiserror::Error)]
#[error("the config transport failed: {0}")]
pub struct ConfigTransportError(String);

impl ConfigTransportError {
    /// Wraps a reason (for the store's log — configuration carries no personal data).
    #[must_use]
    pub fn new(message: impl Into<String>) -> Self {
        Self(message.into())
    }
}

/// The config pull the loop rides ([ADR-0039](../../../docs/adr/0039-config-delivery.md)): fetch the
/// store's effective document, given the version the store already holds. A seam, so the loop is
/// tested without a socket; the field implementation is an HTTPS client authenticated with the
/// store's scoped API key.
pub trait ConfigTransport: Send + Sync {
    /// Fetches the effective config, or `None` if `held_version` is already current — the long-poll
    /// the store initiates ([ADR-0058](../../../docs/adr/0058-config-pull.md)).
    ///
    /// # Errors
    ///
    /// [`ConfigTransportError`] if the cloud could not be reached or its answer did not parse.
    fn fetch(
        &self,
        held_version: Option<&str>,
    ) -> impl Future<Output = Result<Option<SyncedConfig>, ConfigTransportError>> + Send;
}

/// The config-pull client: a transport to the cloud, the running edge to swap into, and the version
/// the store currently holds.
#[derive(Debug)]
pub struct ConfigClient<T, S> {
    transport: T,
    edge: Arc<Edge<S>>,
    held_version: Mutex<Option<String>>,
}

impl<T, S> ConfigClient<T, S>
where
    T: ConfigTransport,
{
    /// Builds a client over a transport and the edge whose session it keeps current.
    pub fn new(transport: T, edge: Arc<Edge<S>>) -> Self {
        Self {
            transport,
            edge,
            held_version: Mutex::new(None),
        }
    }

    /// The version the store currently holds, for the next pull.
    fn held(&self) -> Option<String> {
        self.held_version.lock().expect("held-version lock").clone()
    }

    /// Pulls once; if the cloud has a newer document, rebuilds the session and swaps it into the live
    /// edge, and records the new version. Returns the version applied, or `None` if already current.
    ///
    /// # Errors
    ///
    /// [`ConfigTransportError`] only for a transport failure. A document that does not change the
    /// session still counts as applied (its version is recorded), so the store stops re-pulling it.
    ///
    /// # Panics
    ///
    /// If the held-version lock is poisoned — unreachable, as its critical section only reads or
    /// writes a `String` and cannot itself panic.
    pub async fn pump_once(&self) -> Result<Option<String>, ConfigTransportError> {
        let Some(synced) = self.transport.fetch(self.held().as_deref()).await? else {
            return Ok(None);
        };
        let base = self.edge.session();
        let rebuilt = session_from_config(&base, &synced.document);
        self.edge.apply_session(rebuilt);
        *self.held_version.lock().expect("held-version lock") =
            Some(synced.config_version_id.clone());
        tracing::info!(version = %synced.config_version_id, "applied a new store config");
        Ok(Some(synced.config_version_id))
    }

    /// Runs the config-pull loop until `shutdown` resolves. A transport error is logged and retried
    /// after a backoff — the store keeps trading on its last-known-good session while the link is down.
    pub async fn run<F>(self, shutdown: F)
    where
        F: Future<Output = ()> + Send,
    {
        tokio::pin!(shutdown);
        loop {
            tokio::select! {
                () = &mut shutdown => break,
                result = self.pump_once() => {
                    if let Err(error) = result {
                        tracing::warn!(%error, "config pull failed; backing off");
                        tokio::select! {
                            () = &mut shutdown => break,
                            () = tokio::time::sleep(RETRY_BACKOFF) => {}
                        }
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::session_from_config;
    use pos_proto::SalesChannel;
    use pos_proto::ids::{MenuItemId, TaxClassId};
    use pos_proto::menu::{MenuBook, MenuCatalog, MenuEntry};
    use pos_proto::money::{CurrencyCode, Money};
    use pos_proto::text::DisplayName;
    use pos_proto::ulid::Ulid;

    use crate::app::EdgeSession;

    fn item() -> MenuItemId {
        MenuItemId::new(Ulid::from_u128(500))
    }

    /// A config document with a compiled `menu` node pricing one item on the base session's channel.
    fn document_with_menu(channel: SalesChannel) -> serde_json::Value {
        let catalog = MenuCatalog::new().with(MenuEntry::new(
            item(),
            DisplayName::new("Margherita"),
            Money::new(CurrencyCode::VND, 99_000),
            TaxClassId::new(Ulid::from_u128(1)),
        ));
        let book = MenuBook::new().with(channel, catalog);
        serde_json::json!({ "menu": serde_json::to_value(&book).expect("serialize") })
    }

    #[test]
    fn a_synced_menu_node_becomes_the_sessions_catalog() {
        let base = EdgeSession::bootstrap(); // empty menu, DineIn channel
        assert!(
            base.menu.get(item()).is_none(),
            "the bootstrap menu is empty"
        );
        let document = document_with_menu(base.sales_channel);
        let rebuilt = session_from_config(&base, &document);
        assert!(
            rebuilt.menu.get(item()).is_some(),
            "the item is now priced from the synced menu node"
        );
    }

    #[test]
    fn an_absent_menu_node_leaves_the_menu_unchanged() {
        let base = EdgeSession::bootstrap().with_menu(MenuCatalog::new().with(MenuEntry::new(
            item(),
            DisplayName::new("Existing"),
            Money::new(CurrencyCode::VND, 50_000),
            TaxClassId::new(Ulid::from_u128(1)),
        )));
        // A document with no `menu` node must not blank the trading store's price book.
        let rebuilt = session_from_config(&base, &serde_json::json!({ "other": true }));
        assert!(
            rebuilt.menu.get(item()).is_some(),
            "the existing menu survives a publish that carries no menu node"
        );
    }

    #[test]
    fn a_malformed_menu_node_leaves_the_menu_unchanged() {
        let base = EdgeSession::bootstrap().with_menu(MenuCatalog::new().with(MenuEntry::new(
            item(),
            DisplayName::new("Existing"),
            Money::new(CurrencyCode::VND, 50_000),
            TaxClassId::new(Ulid::from_u128(1)),
        )));
        let rebuilt = session_from_config(&base, &serde_json::json!({ "menu": "not a book" }));
        assert!(
            rebuilt.menu.get(item()).is_some(),
            "a malformed menu node is ignored, not fatal"
        );
    }
}
