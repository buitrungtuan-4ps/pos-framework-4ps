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

use pos_core::business_date::{CutoffHour, StoreTimeZone};
use pos_core::campaign::campaigns_from_published;
use pos_core::capability::{Capability, CapabilityContext};
use pos_core::permission::{Permission, PermissionSet};
use pos_proto::campaign::PublishedCampaigns;
use pos_proto::floor::{FloorPlan, StationPlan};
use pos_proto::locale::TaxRateTable;
use pos_proto::menu::MenuBook;
use pos_proto::money::CurrencyCode;

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

/// The published `locale` node: the store's currency, IANA timezone, and business-date cutoff hour
/// (ADR-0074, Track M4). Each field is applied only if it parses, so a bad value leaves that setting
/// as the base has it rather than blanking it.
#[derive(serde::Deserialize)]
struct PublishedLocale {
    currency_code: String,
    timezone: String,
    cutoff_hour: u8,
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
    // The store's display language from the `locale` node (ADR-0074), if it set one — the locale it
    // renders item names in. Read here so the `menu` branch below can resolve each entry's per-locale
    // name once, at install; a store that sets no language shows each item's default name.
    let display_language = document
        .get("locale")
        .and_then(|locale| locale.get("display_language"))
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|language| !language.is_empty())
        .map(str::to_owned);
    // Parse the node via its JSON text, not `from_value`: some wire types (e.g. `CurrencyCode`)
    // deserialize from a *borrowed* `&str`, which `from_str` supports but `from_value` cannot.
    if let Some(book) = document
        .get("menu")
        .and_then(|value| serde_json::to_string(value).ok())
        .and_then(|text| serde_json::from_str::<MenuBook>(&text).ok())
    {
        let catalog = book.catalog_for(channel);
        // Resolve each entry's display name to the store's language once, here (ADR-0074): the priced
        // line and receipt then read in the store's language, and an item with no translation for it
        // keeps its default name (never-blank).
        session.menu = match display_language.as_deref() {
            Some(language) => catalog.localized(language),
            None => catalog.clone(),
        };
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
    // The `campaigns` node the campaign publish writes (ADR-0077, Track M3): the store's authored
    // promotions, converted to the runtime `Campaign`s the pricing engine (`pos_core::campaign`)
    // evaluates. Absent or unparseable leaves the base list untouched — a bad publish never drops a
    // trading store's promotions. (Applying them to a live bill — building the eval context, timing,
    // and voucher redemption — is the flagged follow-up; this delivers the campaigns to the session.)
    if let Some(published) = document
        .get("campaigns")
        .and_then(|value| serde_json::to_string(value).ok())
        .and_then(|text| serde_json::from_str::<PublishedCampaigns>(&text).ok())
    {
        session.campaigns = campaigns_from_published(&published);
    }
    // The `locale` node the locale publish writes (ADR-0074, Track M4): the store's currency, timezone,
    // and business-date cutoff. Until M4 these were hardcoded to VND/UTC/04:00 in the edge bootstrap.
    // Each field applies only if it parses, so a malformed timezone leaves the running clock alone
    // rather than resetting the store's business date to UTC (the never-blank rule, per field).
    if let Some(locale) = document
        .get("locale")
        .and_then(|value| serde_json::to_string(value).ok())
        .and_then(|text| serde_json::from_str::<PublishedLocale>(&text).ok())
    {
        if let Ok(currency) = CurrencyCode::parse(&locale.currency_code) {
            session.currency = currency;
        }
        if let Ok(timezone) = StoreTimeZone::from_iana_name(&locale.timezone) {
            session.timezone = timezone;
        }
        if let Ok(cutoff) = CutoffHour::new(locale.cutoff_hour) {
            session.cutoff = cutoff;
        }
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
    fn a_campaigns_document_rebuilds_the_session_and_the_engine_prices_it() {
        use pos_core::campaign::{Connectivity, EvalContext, LocalTime, Timing, Weekday, evaluate};
        use pos_proto::campaign::{
            PublishedAction, PublishedCampaign, PublishedCampaignKind, PublishedCampaigns,
            PublishedConditions,
        };
        use pos_proto::ids::CampaignId;
        use pos_proto::money::{Money, Ratio, Rounding};

        let node = PublishedCampaigns::new().with(PublishedCampaign {
            id: CampaignId::new(Ulid::from_u128(0xC1)),
            name: DisplayName::new("10% off the bill"),
            kind: PublishedCampaignKind::BillLevel,
            priority: 0,
            exclusion_group: None,
            action: PublishedAction::Percentage {
                rate: Ratio::percent(10).expect("percent"),
            },
            conditions: PublishedConditions::default(),
            quota_remaining: None,
        });
        let document =
            serde_json::json!({ "campaigns": serde_json::to_value(&node).expect("campaigns") });

        let rebuilt = session_from_config(&EdgeSession::bootstrap(), &document);
        assert_eq!(
            rebuilt.campaigns.len(),
            1,
            "the published campaign reaches the session"
        );

        // The delivered campaign is a real runtime `Campaign`: the engine prices a bill against it —
        // 10% off 100,000 = 10,000 — proving the wire→session→engine path end to end.
        let money = Money::new(CurrencyCode::VND, 100_000);
        let ctx = EvalContext {
            base: money,
            bill_total: money,
            channel: SalesChannel::DineIn,
            now: LocalTime {
                weekday: Weekday::Monday,
                minute_of_day: 12 * 60,
            },
            connectivity: Connectivity::Online,
            rounding: Rounding::HalfUp,
        };
        let applied = evaluate(&rebuilt.campaigns, Timing::PaymentStart, &ctx).expect("evaluate");
        assert_eq!(applied.len(), 1);
        assert_eq!(
            applied.first().map(|reduction| reduction.reduction),
            Some(Money::new(CurrencyCode::VND, 10_000))
        );

        // No `campaigns` node: the base list survives (a bad publish never drops a store's promotions).
        let seeded = EdgeSession::bootstrap().with_campaigns(rebuilt.campaigns.clone());
        let no_node = session_from_config(&seeded, &serde_json::json!({ "other": true }));
        assert_eq!(
            no_node.campaigns.len(),
            1,
            "an absent campaigns node leaves the list"
        );
    }

    #[test]
    fn a_locale_document_rebuilds_currency_timezone_and_cutoff() {
        use pos_core::business_date::CutoffHour;

        let document = serde_json::json!({ "locale": {
            "currency_code": "JPY",
            "timezone": "Asia/Tokyo",
            "cutoff_hour": 6,
        }});
        let rebuilt = session_from_config(&EdgeSession::bootstrap(), &document);
        assert_eq!(rebuilt.currency.as_str(), "JPY");
        assert_eq!(rebuilt.timezone.iana_name(), Some("Asia/Tokyo"));
        assert_eq!(rebuilt.cutoff, CutoffHour::new(6).expect("cutoff"));

        // A malformed timezone leaves the base clock unchanged, but the valid currency still applies —
        // the never-blank rule holds per field.
        let base = EdgeSession::bootstrap();
        let bad = session_from_config(
            &base,
            &serde_json::json!({ "locale": {
                "currency_code": "VND",
                "timezone": "Not/AZone",
                "cutoff_hour": 4,
            }}),
        );
        assert_eq!(
            bad.timezone.iana_name(),
            base.timezone.iana_name(),
            "a bad timezone leaves the running clock"
        );
        assert_eq!(
            bad.currency.as_str(),
            "VND",
            "the valid currency still applied"
        );
    }

    /// A `menu` node whose entry carries per-locale names, plus a `locale` node naming the store's
    /// display language.
    fn document_with_translated_menu(
        channel: SalesChannel,
        language: Option<&str>,
    ) -> serde_json::Value {
        use std::collections::BTreeMap;
        let entry = MenuEntry::new(
            item(),
            DisplayName::new("Margherita"),
            Money::new(CurrencyCode::VND, 99_000),
            TaxClassId::new(Ulid::from_u128(1)),
        )
        .with_name_translations(BTreeMap::from([(
            "vi".to_owned(),
            DisplayName::new("Bánh Margherita"),
        )]));
        let book = MenuBook::new().with(channel, MenuCatalog::new().with(entry));
        let mut document =
            serde_json::json!({ "menu": serde_json::to_value(&book).expect("serialize") });
        if let Some(language) = language {
            document["locale"] = serde_json::json!({ "display_language": language });
        }
        document
    }

    #[test]
    fn the_store_language_localizes_the_menu_and_absent_keeps_the_default() {
        let base = EdgeSession::bootstrap();

        // With the store set to Vietnamese, the item's display name is resolved to its `vi` translation
        // at install, so the priced line and receipt read in the store's language.
        let vietnamese = session_from_config(
            &base,
            &document_with_translated_menu(base.sales_channel, Some("vi")),
        );
        assert_eq!(
            vietnamese
                .menu
                .get(item())
                .expect("priced")
                .display_name
                .as_str(),
            "Bánh Margherita",
        );

        // With no display language, the item keeps its default name (never-blank) — today's behaviour.
        let default = session_from_config(
            &base,
            &document_with_translated_menu(base.sales_channel, None),
        );
        assert_eq!(
            default
                .menu
                .get(item())
                .expect("priced")
                .display_name
                .as_str(),
            "Margherita",
        );

        // A language the item is not translated into also falls back to the default name.
        let untranslated = session_from_config(
            &base,
            &document_with_translated_menu(base.sales_channel, Some("ja")),
        );
        assert_eq!(
            untranslated
                .menu
                .get(item())
                .expect("priced")
                .display_name
                .as_str(),
            "Margherita",
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
