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

use pos_core::capability::{Capability, CapabilityContext};
use pos_core::permission::{Permission, PermissionSet};
use pos_proto::floor::{FloorPlan, StationPlan};
use pos_proto::locale::TaxRateTable;
use pos_proto::menu::MenuBook;

use crate::app::{Edge, EdgeSession, StaffAuth, StaffRoster};

/// How long the loop waits after a transport error before retrying — the store trades locally with
/// its last-known-good session while the cloud link is down, so this is a background reconnect.
const RETRY_BACKOFF: Duration = Duration::from_secs(5);

/// One staff member as the published `permissions` node carries them (ADR-0070). The edge reads only
/// what it authorises against — the `code`, the granted permission ids, and the PIN hash; `id` and
/// `name` are present on the wire but not needed here, and serde ignores them.
#[derive(serde::Deserialize)]
struct PublishedStaff {
    code: String,
    #[serde(default)]
    permissions: Vec<String>,
    #[serde(default)]
    pin_phc: Option<String>,
}

/// The published `permissions` node: the store's staff (ADR-0070).
#[derive(serde::Deserialize)]
struct PublishedPermissions {
    #[serde(default)]
    staff: Vec<PublishedStaff>,
}

/// Maps published permission-id strings to a [`PermissionSet`], dropping any id the running
/// `pos-core` catalogue (§9) does not know — an older edge simply ignores a permission it predates
/// rather than failing to apply the whole node.
fn permission_set_from_ids(ids: &[String]) -> PermissionSet {
    ids.iter()
        .filter_map(|id| Permission::ALL.iter().copied().find(|p| p.meta().id == id))
        .collect()
}

/// Rebuilds an [`EdgeSession`] from a synced config document, on top of a base session.
///
/// Reads the compiled `menu` node (a [`MenuBook`], ADR-0066) and installs the catalog for the
/// session's channel, and the `permissions` node (ADR-0070) into the staff roster the edge authorises
/// against. Any node that is absent or does not parse is left as the base has it — a bad or partial
/// publish never blanks a field, it just does not change it.
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
    // The `permissions` node the people publish writes (ADR-0070) becomes the staff roster the edge
    // authorises sign-ins against, replacing any local roster.
    if let Some(published) = document
        .get("permissions")
        .and_then(|value| serde_json::to_string(value).ok())
        .and_then(|text| serde_json::from_str::<PublishedPermissions>(&text).ok())
    {
        let mut roster = StaffRoster::new();
        for member in published.staff {
            roster.insert(
                member.code,
                StaffAuth {
                    permissions: permission_set_from_ids(&member.permissions),
                    pin_phc: member.pin_phc,
                },
            );
        }
        session.staff = roster;
    }
    // The capability flags the config document carries (ADR-0071): top-level booleans keyed by each
    // capability's key (`tables_enabled`, `pay_first_enabled`, …), read the same way the cloud
    // validator does via the shared `CapabilityContext::from_flags` (§10). Applying them was a silent
    // no-op before M8 — the store now turns table service, KDS, pay-first, etc. on or off from the
    // published profile. Gate on the document carrying at least one known flag key, so a publish that
    // names none leaves the base profile unchanged (the never-blank contract the branches above keep);
    // once it names any, the profile is authoritative and an unnamed flag falls to its declared
    // default.
    if Capability::ALL
        .iter()
        .any(|capability| document.get(capability.meta().key).is_some())
    {
        session.capabilities = CapabilityContext::from_flags(|key| {
            document.get(key).and_then(serde_json::Value::as_bool)
        });
    }
    // The `floor` and `stations` nodes the floor publish writes (ADR-0072): the store's areas/tables
    // and its kitchen stations + item→station routing. Parsed via JSON text like the `menu` node; an
    // absent or unparseable node leaves that plan as the base has it — a bad publish never blanks a
    // trading store's floor or kitchen.
    if let Some(floor) = document
        .get("floor")
        .and_then(|value| serde_json::to_string(value).ok())
        .and_then(|text| serde_json::from_str::<FloorPlan>(&text).ok())
    {
        session.floor = floor;
    }
    if let Some(stations) = document
        .get("stations")
        .and_then(|value| serde_json::to_string(value).ok())
        .and_then(|text| serde_json::from_str::<StationPlan>(&text).ok())
    {
        session.stations = stations;
    }
    // The `tax` node the tax publish writes (ADR-0074, Track M4): the per-(tax class × channel) rate
    // table the edge reprices and bills against. Until M4 this was only ever the hardcoded bootstrap
    // default; a store now bills the authored rates. Absent or unparseable leaves the base table
    // untouched — a bad publish never blanks a trading store's tax to zero (and `rate_for` still
    // refuses an unpriced class rather than charging no tax).
    if let Some(tax_rates) = document
        .get("tax")
        .and_then(|value| serde_json::to_string(value).ok())
        .and_then(|text| serde_json::from_str::<TaxRateTable>(&text).ok())
    {
        session.tax_rates = tax_rates;
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

    /// A real Argon2id PHC of `pin`, with a fixed salt so the test needs no RNG (as `auth.rs` does).
    fn hash_of(pin: &str) -> String {
        use argon2::Argon2;
        use argon2::password_hash::{PasswordHasher as _, SaltString};
        let salt = SaltString::encode_b64(b"a-fixed-test-salt").expect("a valid salt");
        Argon2::default()
            .hash_password(pin.as_bytes(), &salt)
            .expect("hash")
            .to_string()
    }

    /// A `permissions` node with one staff member (code `C01`, one known permission, a PIN hash) plus
    /// a permission id the catalogue does not know (dropped) and a member with no PIN.
    fn document_with_permissions() -> serde_json::Value {
        serde_json::json!({
            "permissions": {
                "store_id": "01STORE000000000000000000",
                "staff": [
                    {
                        "id": "01EMP0000000000000000000A1",
                        "code": "C01",
                        "name": "Alice",
                        "permissions": ["billing.discount.apply", "not.a.real.permission"],
                        "pin_phc": hash_of("2468"),
                    },
                    {
                        "id": "01EMP0000000000000000000A2",
                        "code": "C02",
                        "name": "Bao",
                        "permissions": [],
                        "pin_phc": null,
                    }
                ]
            }
        })
    }

    #[test]
    fn a_permissions_node_becomes_the_staff_roster_and_authorises_sign_in() {
        use pos_core::permission::Permission;

        let base = EdgeSession::bootstrap();
        assert!(base.staff.is_empty(), "the bootstrap roster is empty");

        let rebuilt = session_from_config(&base, &document_with_permissions());
        assert_eq!(rebuilt.staff.len(), 2, "both staff are applied");

        // The known permission id maps in; the unknown one is dropped, not fatal.
        let alice = rebuilt.staff.get("C01").expect("C01 is in the roster");
        assert!(alice.permissions.contains(Permission::ApplyDiscount));
        assert!(!alice.permissions.contains(Permission::VoidFiredLine));

        // A correct PIN authorises and yields the granted set; a wrong PIN does not.
        let granted = rebuilt
            .authorise_staff("C01", "2468")
            .expect("correct PIN authorises");
        assert!(granted.contains(Permission::ApplyDiscount));
        assert!(
            rebuilt.authorise_staff("C01", "0000").is_none(),
            "a wrong PIN yields nothing"
        );
        assert!(
            rebuilt.authorise_staff("NOPE", "2468").is_none(),
            "an unknown code yields nothing"
        );
        assert!(
            rebuilt.authorise_staff("C02", "2468").is_none(),
            "a member with no PIN set cannot sign in"
        );
    }

    #[test]
    fn an_absent_or_malformed_permissions_node_leaves_the_roster_unchanged() {
        use crate::app::{StaffAuth, StaffRoster};
        use pos_core::permission::Permission;

        let mut roster = StaffRoster::new();
        roster.insert(
            "C09",
            StaffAuth {
                permissions: [Permission::AddOpenItem].into_iter().collect(),
                pin_phc: Some(hash_of("1234")),
            },
        );
        let base = EdgeSession::bootstrap().with_staff(roster);

        // No `permissions` node: the existing roster survives.
        let no_node = session_from_config(&base, &serde_json::json!({ "other": true }));
        assert_eq!(no_node.staff.len(), 1);
        assert!(no_node.authorise_staff("C09", "1234").is_some());

        // A malformed node is ignored, not fatal, and does not blank the roster.
        let malformed = session_from_config(&base, &serde_json::json!({ "permissions": "nope" }));
        assert_eq!(
            malformed.staff.len(),
            1,
            "a malformed node leaves the roster unchanged"
        );
    }

    #[test]
    fn capability_flags_in_the_document_rebuild_the_session_profile() {
        use pos_core::capability::Capability;

        // The bootstrap is full-service: tables on, pay-first off, KDS on.
        let base = EdgeSession::bootstrap();
        assert!(base.capabilities.enabled(Capability::Tables));
        assert!(!base.capabilities.enabled(Capability::PayFirst));

        // A publish that flips the store to pay-first (no tables) takes effect — the no-op M8 fixes.
        let rebuilt = session_from_config(
            &base,
            &serde_json::json!({ "tables_enabled": false, "pay_first_enabled": true }),
        );
        assert!(
            !rebuilt.capabilities.enabled(Capability::Tables),
            "table service turned off"
        );
        assert!(
            rebuilt.capabilities.enabled(Capability::PayFirst),
            "pay-first turned on"
        );
        // A flag the document does not name falls to its declared default (KDS defaults on).
        assert!(
            rebuilt.capabilities.enabled(Capability::Kds),
            "an unnamed flag takes its default once the profile names any flag"
        );
    }

    #[test]
    fn a_document_naming_no_capability_flags_leaves_the_profile_unchanged() {
        use pos_core::capability::{Capability, CapabilityContext};

        // Seed a non-default profile (counter: pay-first + queue number + tips, no tables).
        let base = EdgeSession::bootstrap().with_capabilities(CapabilityContext::counter());
        assert!(base.capabilities.enabled(Capability::PayFirst));
        assert!(!base.capabilities.enabled(Capability::Tables));

        // A publish that carries no known flag key must not reset the profile to defaults.
        let rebuilt = session_from_config(&base, &serde_json::json!({ "other": true }));
        assert!(
            rebuilt.capabilities.enabled(Capability::PayFirst),
            "a publish naming no flags leaves the profile unchanged"
        );
        assert!(!rebuilt.capabilities.enabled(Capability::Tables));
    }

    #[test]
    fn a_floor_and_stations_document_rebuild_the_session_plans() {
        use pos_proto::floor::{
            FloorArea, FloorPlan, FloorTable, KitchenStation, RoutingRule, StationPlan,
        };
        use pos_proto::ids::{AreaId, StationId, TableId};

        let table_id = TableId::new(Ulid::from_u128(0xA1));
        let station_id = StationId::new(Ulid::from_u128(1));
        let floor = FloorPlan::new().with(FloorArea {
            area_id: AreaId::new(Ulid::from_u128(1)),
            name: DisplayName::new("Terrace"),
            tables: vec![FloorTable {
                table_id,
                label: DisplayName::new("T1"),
                seats: 4,
                position: None,
            }],
        });
        let stations = StationPlan::new()
            .with_station(KitchenStation {
                station_id,
                name: DisplayName::new("Oven"),
                backup_station_id: None,
            })
            .with_rule(RoutingRule {
                station_id,
                menu_item_id: Some(item()),
                course_id: None,
            });
        let document = serde_json::json!({
            "floor": serde_json::to_value(&floor).expect("floor"),
            "stations": serde_json::to_value(&stations).expect("stations"),
        });

        let rebuilt = session_from_config(&EdgeSession::bootstrap(), &document);
        assert_eq!(rebuilt.floor.tables().count(), 1);
        assert!(rebuilt.floor.table(table_id).is_some());
        // The fired-line resolver derives the station from the published routing (ADR-0072).
        assert_eq!(rebuilt.resolve_station(item(), None), Some(station_id));
    }

    #[test]
    fn a_tax_document_rebuilds_the_session_rate_table() {
        use pos_proto::locale::{TaxRate, TaxRateTable};

        let class = TaxClassId::new(Ulid::from_u128(0x7A));
        let table = TaxRateTable::new()
            .with(class, SalesChannel::DineIn, TaxRate::from_percent(8))
            .with(class, SalesChannel::Takeaway, TaxRate::from_percent(10));
        let document = serde_json::json!({ "tax": serde_json::to_value(&table).expect("tax") });

        let rebuilt = session_from_config(&EdgeSession::bootstrap(), &document);
        assert_eq!(
            rebuilt.tax_rates.rate_for(class, SalesChannel::DineIn),
            Some(TaxRate::from_percent(8))
        );
        assert_eq!(
            rebuilt.tax_rates.rate_for(class, SalesChannel::Takeaway),
            Some(TaxRate::from_percent(10))
        );

        // No `tax` node: the bootstrap default table survives (a bad publish never blanks tax).
        let base = EdgeSession::bootstrap();
        let no_node = session_from_config(&base, &serde_json::json!({ "other": true }));
        assert!(
            !no_node.tax_rates.is_empty(),
            "an absent tax node leaves the table"
        );
    }

    #[test]
    fn an_absent_or_malformed_floor_or_stations_node_leaves_the_plans_unchanged() {
        use pos_proto::floor::{FloorArea, FloorPlan, FloorTable};
        use pos_proto::ids::{AreaId, TableId};

        let table_id = TableId::new(Ulid::from_u128(0xA2));
        let base = EdgeSession::bootstrap().with_floor(FloorPlan::new().with(FloorArea {
            area_id: AreaId::new(Ulid::from_u128(2)),
            name: DisplayName::new("Main"),
            tables: vec![FloorTable {
                table_id,
                label: DisplayName::new("M1"),
                seats: 2,
                position: None,
            }],
        }));

        // No floor/stations node: the seeded floor survives.
        let no_node = session_from_config(&base, &serde_json::json!({ "other": true }));
        assert!(no_node.floor.table(table_id).is_some());

        // A malformed node is ignored, not fatal, and does not blank the plan.
        let malformed = session_from_config(
            &base,
            &serde_json::json!({ "floor": "nope", "stations": 5 }),
        );
        assert!(
            malformed.floor.table(table_id).is_some(),
            "a malformed floor node leaves the plan unchanged"
        );
    }
}
