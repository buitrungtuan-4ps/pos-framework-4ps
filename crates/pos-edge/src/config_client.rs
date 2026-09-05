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
//! Each applied document is also written to the edge's local
//! [`ConfigStore`](pos_ports::ConfigStore), and [`restore_session_from_store`] reads it back at boot
//! before anything binds. That is what makes a store that reboots with its WAN down still able to
//! sell on its real menu: without it every restart came up on [`EdgeSession::bootstrap`] — an empty
//! catalog, an empty roster, an empty floor — and stayed there until the cloud answered, which for
//! an OTA install (which restarts the edge deliberately) or a broadband outage is exactly when it
//! cannot.
//!
//! Scope: the delta form of a config update is still the cloud's to send as a snapshot — this loop
//! pulls whole documents (ADR-0039) and stores them as [`ConfigUpdate::Snapshot`].

use core::future::Future;
use core::time::Duration;
use std::sync::{Arc, Mutex};

use pos_core::business_date::{CutoffHour, StoreTimeZone};
use pos_core::campaign::campaigns_from_published;
use pos_core::capability::{Capability, CapabilityContext};
use pos_core::channels::{accepted_tender, enabled_channels};
use pos_core::inventory::from_published as inventory_from_published;
use pos_core::lease::LeaseConfig;
use pos_core::ota::{DeviceOtaConfig, FleetUpdateConfig};
use pos_core::permission::{Permission, PermissionSet};
use pos_ports::config_store::{ConfigDocument, ConfigSnapshot, ConfigStore, ConfigUpdate};
use pos_ports::error::PortError;
use pos_ports::event_store::EventStore;
use pos_ports::tx::TxContext;
use pos_proto::campaign::PublishedCampaigns;
use pos_proto::channels::{PublishedChannels, PublishedTender};
use pos_proto::devices::PublishedDevices;
use pos_proto::display::LayoutBook;
use pos_proto::floor::{FloorPlan, StationPlan};
use pos_proto::ids::ConfigVersionId;
use pos_proto::inventory::PublishedInventory;
use pos_proto::locale::TaxRateTable;
use pos_proto::menu::MenuBook;
use pos_proto::money::CurrencyCode;
use pos_proto::store_profile::StoreProfile;

use crate::app::{Edge, EdgeSession, StaffAuth, StaffRoster};

/// How long the loop waits after a transport error before retrying — the store trades locally with
/// its last-known-good session while the cloud link is down, so this is a background reconnect.
const RETRY_BACKOFF: Duration = Duration::from_secs(5);

/// One staff member as the published `permissions` node carries them (ADR-0070). The edge reads the
/// `id` (the employee a sign-in acts as, S0b/ADR-0084), the `code` a person types, the granted
/// permission ids, and the PIN hash. `name` is present on the wire but not used here, and serde
/// ignores it.
#[derive(serde::Deserialize)]
struct PublishedStaff {
    #[serde(default)]
    id: Option<String>,
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
    /// Whether this store quotes tax-inclusive prices (ADR-0104). `#[serde(default)]` so a locale
    /// node published before this field existed still applies, as the exclusive posture it meant.
    #[serde(default)]
    prices_include_tax: bool,
    /// What the grand total is rounded to in cash, in minor units (ADR-0105). Absent means no
    /// rounding, which is what every store did before the field existed.
    #[serde(default)]
    cash_rounding_increment: Option<i64>,
    /// The notes the till offers as quick-cash keys, in minor units. Absent means the exact amount
    /// only, which is the front end's own fallback, so an older publish changes nothing.
    #[serde(default)]
    cash_denominations: Vec<i64>,
    /// How many days the store keeps a personal record before its own sweep scrubs it (ADR-0107).
    /// Absent leaves the session's current figure — the country pack's default — rather than zero,
    /// which would scrub a buyer the moment the invoice was printed.
    #[serde(default)]
    default_retention_days: Option<u16>,
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
#[expect(
    clippy::too_many_lines,
    reason = "a flat series of independent, self-contained node branches (menu, permissions, \
              capabilities, floor, stations, tax, campaigns, inventory, channels, tender, qr, \
              locale, fleet_update, device_ota, lease); each is a few lines and reads better inline \
              behind a helper indirection"
)]
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
                    // The employee a sign-in under this code acts as (S0b/ADR-0084). A missing or
                    // malformed id leaves it `None`, and the roster then refuses to sign that code in
                    // rather than acting as a fabricated identity.
                    employee_id: member
                        .id
                        .as_deref()
                        .and_then(|id| id.parse::<pos_proto::ids::EmployeeId>().ok()),
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
    // The `layout` node the catalog publish writes beside `menu` (ADR-0066): how the till groups and
    // orders its buttons, resolved here for the store's channel exactly as the price book is. Until
    // this branch the node was authored, validated, published — and read by nobody (C4). Absent or
    // unparseable leaves the plan as the base has it: a bad publish never blanks a trading till's
    // buttons, and an empty plan means the till draws the flat price book instead.
    if let Some(book) = document
        .get("layout")
        .and_then(|value| serde_json::to_string(value).ok())
        .and_then(|text| serde_json::from_str::<LayoutBook>(&text).ok())
    {
        session.layout = book.plan_for(channel).clone();
    }
    // The `devices` node the device publish writes (ADR-0100): the printers and kitchen displays this
    // store may address. Absent leaves the session's set as the base has it — which for a box that
    // has never synced means empty, and empty means "print nothing", not "print blind".
    if let Some(devices) = document
        .get("devices")
        .and_then(|value| serde_json::to_string(value).ok())
        .and_then(|text| serde_json::from_str::<PublishedDevices>(&text).ok())
    {
        session.devices = devices;
    }
    // The `store_profile` node (ADR-0106): who this store legally is, as the receipt prints it. An
    // absent or unparseable node leaves the previous profile in place, which is the never-blank rule
    // applied where it matters most — a store mid-service must not start printing receipts with no
    // name because one publish was malformed.
    if let Some(profile) = document
        .get("store_profile")
        .and_then(|value| serde_json::to_string(value).ok())
        .and_then(|text| serde_json::from_str::<StoreProfile>(&text).ok())
    {
        session.profile = profile;
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
    // The `inventory` node the inventory publish writes (ADR-0079, Track M6): the store's ingredients,
    // per-item recipes (bill of materials), and auto-86 thresholds, converted to the runtime RecipeBook
    // the fire path consumes and a per-item threshold map. Absent or unparseable leaves the base book
    // untouched — a bad publish never blanks a trading store's recipes. This kills the empty-book
    // bootstrap: a fired line now consumes its ingredients (`decide_line` reads `session.recipes`).
    // Deriving live item availability (auto-86 on the menu) needs an on-hand stock projection, which
    // arrives with the flagged goods-in/stocktake follow-up; the thresholds are delivered here so that
    // slice has them, and `EdgeSession::item_sellable` is the pure decision it will drive.
    if let Some(published) = document
        .get("inventory")
        .and_then(|value| serde_json::to_string(value).ok())
        .and_then(|text| serde_json::from_str::<PublishedInventory>(&text).ok())
    {
        let (book, thresholds) = inventory_from_published(&published);
        session.recipes = book;
        session.recipe_thresholds = thresholds;
    }
    // The `channels` node the channels publish writes (ADR-0080, Track M7): the sales channels a store
    // accepts. A present node is authoritative — the edge refuses an order on a channel it does not
    // list; an absent node leaves `None` (no restriction), so a store that never published one trades
    // on every channel exactly as before M7.
    if let Some(published) = document
        .get("channels")
        .and_then(|value| serde_json::to_string(value).ok())
        .and_then(|text| serde_json::from_str::<PublishedChannels>(&text).ok())
    {
        session.enabled_channels = Some(enabled_channels(&published));
    }
    // The `tender` node the tender publish writes (ADR-0080, Track M7): the payment methods a store
    // accepts. Same opt-in rule — a present node gates settlement, an absent one accepts any known
    // method as before.
    if let Some(published) = document
        .get("tender")
        .and_then(|value| serde_json::to_string(value).ok())
        .and_then(|text| serde_json::from_str::<PublishedTender>(&text).ok())
    {
        session.accepted_tender = Some(accepted_tender(&published));
    }
    // The `qr` guardrail node (P11b, authored via ADR-0080, Track M7): the edge reads only
    // `staff_confirmation_required` — whether a table-bearing QR order waits for staff before the
    // kitchen sees it. The cloud reads the same node for its own guardrails (enabled/hours/rate). An
    // absent field leaves the base value (default `true`, ADR-0057), so a store that never published
    // the node still holds guest orders for staff.
    if let Some(required) = document
        .get("qr")
        .and_then(|node| node.get("staff_confirmation_required"))
        .and_then(serde_json::Value::as_bool)
    {
        session.qr_staff_confirmation_required = required;
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
        // A bool cannot fail to parse, so unlike its siblings this one applies unconditionally —
        // which is also what makes turning the posture back off a publish rather than a release.
        session.prices_include_tax = locale.prices_include_tax;
        // Likewise: both have a `serde(default)` that *is* the previous behaviour, so applying them
        // unconditionally is what lets a store turn cash rounding off, or clear its quick-cash keys,
        // by publishing — rather than by waiting for a build (ADR-0105). A non-positive increment is
        // dropped rather than obeyed: `round_to_increment` needs a non-zero step, and a negative one
        // is a typo nobody means.
        session.cash_rounding_increment = locale
            .cash_rounding_increment
            .filter(|increment| *increment > 0);
        session
            .cash_denominations
            .clone_from(&locale.cash_denominations);
        // Absent, or zero, leaves what the store already had: a published zero would mean "scrub a
        // buyer's record the instant the invoice is printed", which is never what an operator means
        // and would destroy the document's own evidence (ADR-0107).
        if let Some(days) = locale.default_retention_days.filter(|days| *days > 0) {
            session.retention_days = days;
        }
    }
    // The `fleet_update` node the OTA publish writes (ADR-0048): the rollout every device weighs
    // itself against, and the signing keys revocation has retired. The cloud has published this node
    // since P9e-3 and nothing here read it, so the rollout decision had no rollout to decide about.
    //
    // The parse is two-stage — serde into the wire form, then `validate` into the domain form — and
    // both stages are recoverable in the same way: an absent, unparseable, or invalid node leaves the
    // previous rollout untouched. That is the right failure for this node specifically. A device that
    // keeps yesterday's rollout installs an update it was already eligible for; a device that lost it
    // would stop being eligible for anything, which turns one bad publish into a fleet that no longer
    // takes security fixes. Halting a rollout is `halted` inside the node, never a deletion, so
    // stopping the fleet does not depend on a node vanishing.
    //
    // A violation reaching here means the cloud published something its own validator would refuse
    // (the same `FleetUpdateConfig::validate` runs on both sides), so it is logged rather than
    // swallowed. Config carries no personal data, so the violations themselves are safe to log.
    if let Some(config) = document
        .get("fleet_update")
        .and_then(|value| serde_json::to_string(value).ok())
        .and_then(|text| serde_json::from_str::<FleetUpdateConfig>(&text).ok())
    {
        match config.validate() {
            Ok(rollout) => session.fleet_update = Some(rollout),
            Err(violations) => tracing::warn!(
                violations = %violations.join("; "),
                "the published fleet_update node did not validate; keeping the previous rollout"
            ),
        }
    }
    // The `device_ota` node (ADR-0048): where the cloud has placed *this* device — its ring and its
    // stable canary bucket. Stable is the point: the bucket fixes the device's position in the canary
    // ramp, so a device does not cross the rollout threshold and back again as the percentage moves,
    // and the cloud owning it is what makes it survive a reboot.
    //
    // Same never-blank rule as every node above, and here it is what keeps a device *placed*: losing
    // the placement would leave it eligible for nothing at all. A device the cloud has never placed
    // stays `None`, which is the safe end of that trade rather than an accident — see the field's own
    // doc for why no default ring is invented.
    if let Some(config) = document
        .get("device_ota")
        .and_then(|value| serde_json::to_string(value).ok())
        .and_then(|text| serde_json::from_str::<DeviceOtaConfig>(&text).ok())
    {
        match config.validate() {
            Ok(assignment) => session.device_ota = Some(assignment),
            Err(violations) => tracing::warn!(
                violations = %violations.join("; "),
                "the published device_ota node did not validate; keeping the previous placement"
            ),
        }
    }
    // The `lease` node (ADR-0108): the store's *authoritative* lease generation — the number this
    // box's own held generation is weighed against to decide whether it is still the store. Derived
    // by the cloud from its `store_lease` row, never authored by anybody.
    //
    // Same never-blank rule, and it is load-bearing in a way the others are not: a malformed or
    // absent node must not un-supersede a machine, so an unparseable value keeps the last good
    // generation rather than clearing it back to "no lease issued". A store the cloud has never
    // issued a lease to stays `None`, which reads as active — the fleet's behaviour before this node
    // existed, preserved on purpose.
    if let Some(config) = document
        .get("lease")
        .and_then(|value| serde_json::to_string(value).ok())
        .and_then(|text| serde_json::from_str::<LeaseConfig>(&text).ok())
    {
        session.lease_generation = Some(config.generation());
    } else if document.get("lease").is_some() {
        tracing::warn!(
            "the published lease node did not parse; keeping the previous lease generation"
        );
    }
    session
}

/// Restores the live session from the configuration this store last synced, so a box that reboots
/// with its WAN down still trades on its real menu ([ADR-0004](../../../docs/adr/0004-cloud-owned-configuration.md)).
///
/// Reads the local [`ConfigStore`]: [`ConfigStore::current`] first, then falling back to
/// [`ConfigStore::last_known_good`] — the two differ only after a rejected version, and the version
/// that validated is the right thing to come back on. Returns the version now live, or `None` when
/// this box has never synced (a first boot, which trades on [`EdgeSession::bootstrap`] until the
/// first pull answers) — which is also what the config-pull loop should start out holding, so a
/// restart does not re-pull a document it already has.
///
/// Never fails the boot: a stored document that will not parse is logged and skipped, because a
/// store that will not start is worse than a store on a stale menu.
///
/// # Errors
///
/// [`PortError`] if the local store cannot be read at all — which is the same fault that would stop
/// the event log opening, so the caller treats it as one.
pub async fn restore_session_from_store<S>(edge: &Edge<S>) -> Result<Option<String>, PortError>
where
    S: EventStore + ConfigStore,
{
    let store_id = edge.store_id();
    let store = edge.store();
    let stored = match store.current(store_id).await? {
        Some(snapshot) => Some(snapshot),
        None => store.last_known_good(store_id).await?,
    };
    let Some(stored) = stored else {
        tracing::info!("no configuration has been synced to this store yet; trading on defaults");
        return Ok(None);
    };
    let version = stored.config_version_id.to_string();
    let Ok(document) = serde_json::from_str::<serde_json::Value>(stored.document.as_json()) else {
        // Unreachable through the port (a `ConfigDocument` holds validated JSON), so this is the
        // hand-edited-database case. Logged without the document: config carries no personal data,
        // but a whole document in a log line is noise nobody reads.
        tracing::warn!(%version, "the stored configuration is not JSON; trading on defaults");
        return Ok(None);
    };
    let rebuilt = session_from_config(&edge.session(), &document);
    edge.apply_session(rebuilt);
    tracing::info!(%version, "restored the last synced configuration from local storage");
    Ok(Some(version))
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
    /// Fetches the effective config, or `None` if `held_version` is already current — the pull the
    /// store initiates ([ADR-0039](../../../docs/adr/0039-config-delivery.md)).
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
    S: EventStore + ConfigStore,
{
    /// Builds a client over a transport and the edge whose session it keeps current.
    ///
    /// `held_version` is what the store already has on disk — the value
    /// [`restore_session_from_store`] returned at boot — so the first pull asks for a *change*
    /// rather than re-fetching a document the store is already running. `None` on a box that has
    /// never synced.
    pub fn new(transport: T, edge: Arc<Edge<S>>, held_version: Option<String>) -> Self {
        Self {
            transport,
            edge,
            held_version: Mutex::new(held_version),
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
        // Store it before recording the version: the live session is already swapped either way, and
        // what this buys is the *next* boot. A failure here is a degradation (the box will re-pull
        // after a restart), not a reason to refuse a document the counter is already selling on.
        self.persist(&synced).await;
        *self.held_version.lock().expect("held-version lock") =
            Some(synced.config_version_id.clone());
        tracing::info!(version = %synced.config_version_id, "applied a new store config");
        Ok(Some(synced.config_version_id))
    }

    /// Writes an applied document to the store's local [`ConfigStore`], so the next boot restores it
    /// instead of coming up on defaults.
    ///
    /// Every failure is logged and swallowed. The document has already been applied to the live
    /// session, so the only thing lost is the offline restart, and losing that quietly beats a
    /// transport error that would make the loop back off and stop pulling.
    async fn persist(&self, synced: &SyncedConfig) {
        let Ok(config_version_id) = synced.config_version_id.parse::<ConfigVersionId>() else {
            tracing::warn!(
                version = %synced.config_version_id,
                "the cloud's config version is not a ULID; applied but not stored, so a restart re-pulls it"
            );
            return;
        };
        let document = match serde_json::value::to_raw_value(&synced.document) {
            Ok(raw) => ConfigDocument::new(raw),
            Err(error) => {
                tracing::warn!(%error, "the applied config document could not be re-encoded to store");
                return;
            }
        };
        let update = ConfigUpdate::Snapshot(ConfigSnapshot {
            config_version_id,
            store_id: self.edge.store_id(),
            document,
        });
        let store = self.edge.store();
        let stored = async {
            let mut tx = store.begin().await?;
            store.apply(&mut tx, &update).await?;
            tx.commit().await
        }
        .await;
        if let Err(error) = stored {
            tracing::warn!(
                %error,
                version = %synced.config_version_id,
                "applied the new config but could not store it; a restart will re-pull it"
            );
        }
    }

    /// Runs the config-pull loop until `shutdown` resolves: pull, apply anything new, wait
    /// `poll_interval`, repeat. A transport error is logged and retried after a shorter backoff
    /// instead — the store keeps trading on its last-known-good session while the link is down.
    ///
    /// The cloud answers a pull immediately (it does not yet park the connection until the config
    /// changes), so the loop paces itself with `poll_interval` rather than pulling in a tight spin;
    /// a published change reaches the counter within one interval. Server-side long-poll is a later
    /// optimisation ([ADR-0039](../../../docs/adr/0039-config-delivery.md)); the seam does not change
    /// when it lands. Both the interval wait and the error backoff are cut short by `shutdown`, so a
    /// stop drains promptly.
    pub async fn run<F>(self, poll_interval: Duration, shutdown: F)
    where
        F: Future<Output = ()> + Send,
    {
        tokio::pin!(shutdown);
        loop {
            tokio::select! {
                () = &mut shutdown => break,
                result = self.pump_once() => {
                    let wait = match result {
                        Ok(_) => poll_interval,
                        Err(error) => {
                            tracing::warn!(%error, "config pull failed; backing off");
                            RETRY_BACKOFF
                        }
                    };
                    tokio::select! {
                        () = &mut shutdown => break,
                        () = tokio::time::sleep(wait) => {}
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
    fn a_synced_layout_node_becomes_the_tills_button_plan() {
        // C4: the node was authored, validated, published — and read by nobody, so every till in the
        // fleet drew the flat price book whatever an operator arranged.
        use pos_proto::display::{DisplayButton, DisplayCategory, DisplayPlan, LayoutBook};
        use pos_proto::ids::DisplayCategoryId;

        let base = EdgeSession::bootstrap();
        assert!(base.layout.is_empty(), "the bootstrap lays nothing out");

        let plan = DisplayPlan::new().with(DisplayCategory {
            display_category_id: DisplayCategoryId::new(Ulid::from_u128(3)),
            name: DisplayName::new("Pizza"),
            buttons: vec![DisplayButton {
                menu_item_id: item(),
                label: DisplayName::new("Margherita"),
                position: None,
            }],
            subcategories: Vec::new(),
        });
        let book = LayoutBook::new().with(base.sales_channel, plan);
        let document =
            serde_json::json!({ "layout": serde_json::to_value(&book).expect("serialize") });

        let rebuilt = session_from_config(&base, &document);
        let category = rebuilt.layout.categories().first().expect("one category");
        assert_eq!(category.name.as_str(), "Pizza");
        assert_eq!(
            category.buttons.first().map(|button| button.menu_item_id),
            Some(item())
        );
    }

    #[test]
    fn a_layout_published_for_another_channel_does_not_lay_out_this_one() {
        // The same guard the price book keeps: a Grab layout must not arrange the till.
        use pos_proto::display::{DisplayCategory, DisplayPlan, LayoutBook};
        use pos_proto::ids::DisplayCategoryId;

        let base = EdgeSession::bootstrap(); // DineIn
        let plan = DisplayPlan::new().with(DisplayCategory {
            display_category_id: DisplayCategoryId::new(Ulid::from_u128(3)),
            name: DisplayName::new("Grab"),
            buttons: Vec::new(),
            subcategories: Vec::new(),
        });
        let book = LayoutBook::new().with(SalesChannel::Delivery, plan);
        let document =
            serde_json::json!({ "layout": serde_json::to_value(&book).expect("serialize") });

        let rebuilt = session_from_config(&base, &document);
        assert!(
            rebuilt.layout.is_empty(),
            "the till falls back to the flat price book, not another channel's arrangement"
        );
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

        // The published employee id is read, so a sign-in acts as the real person (S0b/ADR-0084).
        assert_eq!(
            alice.employee_id,
            Some(
                "01EMP0000000000000000000A1"
                    .parse::<pos_proto::ids::EmployeeId>()
                    .expect("a valid employee ULID")
            ),
        );

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
                employee_id: None,
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
    fn an_inventory_document_rebuilds_the_recipe_book_and_drives_auto_86() {
        use pos_core::inventory::StockProjection;
        use pos_proto::enums::UnitOfMeasure;
        use pos_proto::ids::IngredientId;
        use pos_proto::inventory::{
            PublishedIngredient, PublishedInventory, PublishedRecipe, PublishedRecipeLine,
        };
        use pos_proto::quantity::Quantity;
        use pos_proto::wire_enum::Open;

        let dough = IngredientId::new(Ulid::from_u128(0xD0));
        // One item needs 100 g of dough per unit, and is 86'd at or below 2 makeable.
        let node = PublishedInventory::from_parts(
            vec![PublishedIngredient {
                id: dough,
                name: DisplayName::new("Dough"),
                unit: Open::from_known(UnitOfMeasure::Gram),
            }],
            vec![PublishedRecipe {
                item: item(),
                lines: vec![PublishedRecipeLine {
                    ingredient: dough,
                    per_unit: Quantity::from_milli(100_000),
                }],
                auto_86_threshold: 2,
            }],
            Vec::new(),
        );
        let document =
            serde_json::json!({ "inventory": serde_json::to_value(&node).expect("inventory") });

        let rebuilt = session_from_config(&EdgeSession::bootstrap(), &document);
        assert_eq!(rebuilt.recipe_thresholds.get(&item()).copied(), Some(2));

        // The delivered recipe is a real runtime `RecipeBook`: against a stock projection, the auto-86
        // decision follows §8 — 250 g makes 2 (not strictly above the threshold of 2, so 86'd), 350 g
        // makes 3 (above it, so sellable). This proves the wire→session→availability path.
        let mut low = StockProjection::new();
        low.set_on_hand(dough, Quantity::from_milli(250_000));
        assert!(
            !rebuilt.item_sellable(item(), &low),
            "2 makeable is not strictly above the threshold of 2"
        );
        let mut high = StockProjection::new();
        high.set_on_hand(dough, Quantity::from_milli(350_000));
        assert!(
            rebuilt.item_sellable(item(), &high),
            "3 makeable is above the threshold, so sellable"
        );

        // An item with no recipe is always sellable, whatever the stock.
        let untracked = MenuItemId::new(Ulid::from_u128(0x999));
        assert!(rebuilt.item_sellable(untracked, &StockProjection::new()));

        // No `inventory` node: the base book stays empty (a bad publish never blanks recipes).
        let no_node =
            session_from_config(&EdgeSession::bootstrap(), &serde_json::json!({ "x": 1 }));
        assert!(no_node.recipe_thresholds.is_empty());
    }

    #[test]
    fn channels_and_tender_nodes_gate_the_session_and_absent_means_no_restriction() {
        use pos_proto::{PaymentMethod, SalesChannel};

        // A store with no nodes published has no restriction — every channel and tender is accepted.
        let base = EdgeSession::bootstrap();
        assert!(base.channel_enabled(SalesChannel::Delivery));
        assert!(base.tender_accepted(PaymentMethod::Card));

        // Publishing `channels` = [DINE_IN] and `tender` = [CASH] makes each authoritative.
        let document = serde_json::json!({
            "channels": { "enabled": ["SALES_CHANNEL_DINE_IN"] },
            "tender": { "accepted": ["PAYMENT_METHOD_CASH"] },
        });
        let rebuilt = session_from_config(&base, &document);
        assert!(rebuilt.channel_enabled(SalesChannel::DineIn));
        assert!(
            !rebuilt.channel_enabled(SalesChannel::Delivery),
            "a channel not listed is refused once a node is published"
        );
        assert!(rebuilt.tender_accepted(PaymentMethod::Cash));
        assert!(
            !rebuilt.tender_accepted(PaymentMethod::Card),
            "a method not listed is refused once a node is published"
        );

        // A publish that carries neither node leaves the prior restriction untouched (never-blank).
        let unchanged = session_from_config(&rebuilt, &serde_json::json!({ "other": true }));
        assert!(!unchanged.channel_enabled(SalesChannel::Delivery));
        assert!(!unchanged.tender_accepted(PaymentMethod::Card));
    }

    #[test]
    fn a_qr_node_toggles_staff_confirmation_and_defaults_on() {
        // The bootstrap holds guest orders for staff (ADR-0057).
        let base = EdgeSession::bootstrap();
        assert!(base.qr_staff_confirmation_required);

        // Publishing `qr.staff_confirmation_required = false` turns the hold off (ADR-0080, M7).
        let off = session_from_config(
            &base,
            &serde_json::json!({ "qr": { "staff_confirmation_required": false } }),
        );
        assert!(!off.qr_staff_confirmation_required);

        // A publish with no `qr` node leaves the prior value untouched (never-blank).
        let unchanged = session_from_config(&off, &serde_json::json!({ "other": true }));
        assert!(!unchanged.qr_staff_confirmation_required);
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

    #[test]
    fn a_locale_document_carries_the_country_s_till_money() {
        // ADR-0105: cash rounding and the notes a guest carries are country facts, published rather
        // than compiled in. `bill_input` passed a literal `None` for rounding until this arrived.
        let india = session_from_config(
            &EdgeSession::bootstrap(),
            &serde_json::json!({ "locale": {
                "currency_code": "INR",
                "timezone": "Asia/Kolkata",
                "cutoff_hour": 4,
                "prices_include_tax": true,
                "cash_rounding_increment": 100,
                "cash_denominations": [1_000, 2_000, 5_000, 10_000],
            }}),
        );
        assert_eq!(india.cash_rounding_increment, Some(100), "₹1 in paise");
        assert_eq!(india.cash_denominations, vec![1_000, 2_000, 5_000, 10_000]);
        assert!(india.prices_include_tax, "MRP is inclusive");

        // Japan publishes no increment at all, and that has to *clear* a previously rounding store
        // rather than leave it — otherwise a box could never stop rounding without a release.
        let japan = session_from_config(
            &india,
            &serde_json::json!({ "locale": {
                "currency_code": "JPY",
                "timezone": "Asia/Tokyo",
                "cutoff_hour": 6,
                "prices_include_tax": true,
                "cash_denominations": [1_000, 5_000, 10_000],
            }}),
        );
        assert_eq!(
            japan.cash_rounding_increment, None,
            "the 1-yen coin circulates, so there is nothing to round to"
        );
        assert_eq!(japan.cash_denominations, vec![1_000, 5_000, 10_000]);
    }

    #[test]
    fn a_locale_node_published_before_the_till_money_existed_still_applies() {
        // The additive-in-both-directions property: a node written by an older cloud carries none of
        // the three keys, and a newer edge reading it gets the behaviour that node meant — exclusive
        // prices, no rounding, no quick-cash keys.
        let rebuilt = session_from_config(
            &EdgeSession::bootstrap(),
            &serde_json::json!({ "locale": {
                "currency_code": "VND",
                "timezone": "Asia/Ho_Chi_Minh",
                "cutoff_hour": 4,
            }}),
        );
        assert_eq!(rebuilt.currency.as_str(), "VND");
        assert!(!rebuilt.prices_include_tax);
        assert_eq!(rebuilt.cash_rounding_increment, None);
        assert!(rebuilt.cash_denominations.is_empty());
    }

    #[test]
    fn a_rounding_increment_of_zero_is_dropped_rather_than_obeyed() {
        // `round_to_increment` needs a non-zero step, and a negative one is a typo nobody means. The
        // cloud refuses both at the publish; this is the second line, because a store may be reading
        // a node an older or hand-edited publish wrote.
        for bad in [0, -500] {
            let rebuilt = session_from_config(
                &EdgeSession::bootstrap(),
                &serde_json::json!({ "locale": {
                    "currency_code": "VND",
                    "timezone": "Asia/Ho_Chi_Minh",
                    "cutoff_hour": 4,
                    "cash_rounding_increment": bad,
                }}),
            );
            assert_eq!(
                rebuilt.cash_rounding_increment, None,
                "{bad} is not an increment"
            );
        }
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

    /// A `fleet_update` node rolling out 1.4.0 to the fleet ring at 40 %, plus a `device_ota` node
    /// placing this device in the fleet at bucket 10 — inside the ramp.
    fn document_with_ota() -> serde_json::Value {
        serde_json::json!({
            "fleet_update": {
                "target_version": "1.4.0",
                "min_ring": "fleet",
                "rollout_percent": 40,
                "signing_key_id": "a1a1a1a1a1a1a1a1",
                "revoked_key_ids": ["b2b2b2b2b2b2b2b2"],
            },
            "device_ota": { "ring": "fleet", "canary_bucket": 10 },
        })
    }

    #[test]
    fn the_ota_nodes_deliver_the_rollout_and_this_devices_placement() {
        use pos_core::ota::{
            DeviceState, ReleaseVersion, Ring, RolloutDecision, decide_rollout,
            parse_signing_key_id,
        };

        // Before this slice the edge read neither node, so both were dark whatever the cloud published.
        let base = EdgeSession::bootstrap();
        assert!(base.fleet_update.is_none(), "no rollout in the bootstrap");
        assert!(base.device_ota.is_none(), "no placement in the bootstrap");

        let rebuilt = session_from_config(&base, &document_with_ota());
        let rollout = rebuilt.fleet_update.as_ref().expect("a rollout arrived");
        assert_eq!(rollout.update.target, ReleaseVersion::new(1, 4, 0));
        assert_eq!(rollout.update.min_ring, Ring::Fleet);
        assert_eq!(rollout.update.fleet_rollout_percent, 40);
        assert!(!rollout.update.halted);
        assert_eq!(
            rollout.revoked_keys,
            vec![parse_signing_key_id("b2b2b2b2b2b2b2b2").expect("a key id")],
            "the retired key travels with the rollout it constrains"
        );
        let placement = rebuilt.device_ota.expect("a placement arrived");
        assert_eq!(placement.ring, Ring::Fleet);
        assert_eq!(placement.canary_bucket, 10);

        // The delivered values are the real domain inputs: `decide_rollout` weighs this device against
        // this rollout and says install — bucket 10 is inside a 40 % ramp — which proves the
        // wire→session→decision path, not just that two fields deserialize.
        let device = DeviceState {
            current: ReleaseVersion::new(1, 3, 0),
            ring: placement.ring,
            canary_bucket: placement.canary_bucket,
            last_self_test: None,
        };
        assert_eq!(
            decide_rollout(&device, &rollout.update, &rollout.revoked_keys),
            RolloutDecision::Install
        );

        // And the halt lever the console pulls arrives the same way, outranking eligibility.
        let mut halted = document_with_ota();
        halted["fleet_update"]["halted"] = serde_json::json!(true);
        let paused = session_from_config(&base, &halted);
        let paused_rollout = paused.fleet_update.as_ref().expect("a rollout arrived");
        assert_eq!(
            decide_rollout(
                &device,
                &paused_rollout.update,
                &paused_rollout.revoked_keys
            ),
            RolloutDecision::Halt
        );
    }

    #[test]
    fn an_absent_or_invalid_ota_node_leaves_the_rollout_and_placement_untouched() {
        use pos_core::ota::{ReleaseVersion, Ring};

        let seeded = session_from_config(&EdgeSession::bootstrap(), &document_with_ota());

        // A publish carrying neither node leaves both — a device that lost its rollout or its
        // placement would stop being eligible for anything, so one bad publish would strand the fleet
        // off security fixes. Never-blank matters more here than anywhere else in this function.
        let no_node = session_from_config(&seeded, &serde_json::json!({ "other": true }));
        assert_eq!(
            no_node
                .fleet_update
                .as_ref()
                .map(|rollout| rollout.update.target),
            Some(ReleaseVersion::new(1, 4, 0))
        );
        assert_eq!(
            no_node.device_ota.map(|placement| placement.ring),
            Some(Ring::Fleet)
        );

        // Nodes that parse as JSON but fail `validate` (a ring that names no ring, a percent above
        // 100) are refused at the domain boundary and leave the previous values, not half of them.
        let invalid = session_from_config(
            &seeded,
            &serde_json::json!({
                "fleet_update": {
                    "target_version": "1.9.0",
                    "min_ring": "everyone",
                    "rollout_percent": 250,
                    "signing_key_id": "a1a1a1a1a1a1a1a1",
                },
                "device_ota": { "ring": "fleet", "canary_bucket": 200 },
            }),
        );
        assert_eq!(
            invalid
                .fleet_update
                .as_ref()
                .map(|rollout| rollout.update.target),
            Some(ReleaseVersion::new(1, 4, 0)),
            "an invalid rollout does not become the rollout"
        );
        assert_eq!(
            invalid.device_ota.map(|placement| placement.canary_bucket),
            Some(10),
            "an out-of-range bucket does not become the placement"
        );

        // Nodes that are not even the right JSON shape are ignored the same way, not fatal.
        let malformed = session_from_config(
            &seeded,
            &serde_json::json!({ "fleet_update": "nope", "device_ota": 7 }),
        );
        assert!(malformed.fleet_update.is_some());
        assert!(malformed.device_ota.is_some());
    }

    #[test]
    fn no_placement_is_the_safe_state_because_every_default_ring_is_more_exposed() {
        use pos_core::ota::{
            DeviceState, ReleaseVersion, Ring, RolloutDecision, SkipReason, decide_rollout,
        };

        // A rollout at its first stage: open to the lab ring only, with the fleet ramp still at 0 %.
        // This is the least-proven moment in any update's life — it has reached the test cohort and
        // nothing else.
        let document = serde_json::json!({
            "fleet_update": {
                "target_version": "1.4.0",
                "min_ring": "lab",
                "rollout_percent": 0,
                "signing_key_id": "a1a1a1a1a1a1a1a1",
            },
        });
        let session = session_from_config(&EdgeSession::bootstrap(), &document);
        let rollout = session.fleet_update.as_ref().expect("a rollout arrived");
        assert!(
            session.device_ota.is_none(),
            "a document that places no device leaves the placement unset"
        );

        let placed = |ring, canary_bucket| DeviceState {
            current: ReleaseVersion::new(1, 3, 0),
            ring,
            canary_bucket,
            last_self_test: None,
        };

        // This is why `device_ota` is an `Option` and not a field with a default. `Ring::Lab` reads
        // like the cautious choice and is the least cautious one available: lab is the first ring a
        // rollout opens to and it is exempt from the canary ramp entirely, so a device defaulted to
        // Lab installs at the stage where an update has been proven on nothing. It would be first in
        // line, not last.
        assert_eq!(
            decide_rollout(
                &placed(Ring::Lab, 0),
                &rollout.update,
                &rollout.revoked_keys
            ),
            RolloutDecision::Install,
            "a Lab default would take a lab-only, unramped update immediately"
        );
        // A real fleet placement waits for the ramp to reach its bucket. Waiting is the whole
        // behaviour a rollout exists to produce, and it is only available to a device the cloud has
        // actually placed — `min_ring` is a floor, so this same lab-stage rollout is already visible
        // to every ring and the ramp is the only thing holding the fleet back.
        assert_eq!(
            decide_rollout(
                &placed(Ring::Fleet, 0),
                &rollout.update,
                &rollout.revoked_keys
            ),
            RolloutDecision::Skip(SkipReason::NotInCanaryYet),
            "a placed fleet device waits for its bucket"
        );
        // Which is why bucket 0 is not a safe default either: it is the first fleet device in, at the
        // very first point of the ramp. There is no placement that means "wait for a real one".
        let ramping = pos_core::ota::PublishedUpdate {
            fleet_rollout_percent: 1,
            ..rollout.update
        };
        assert_eq!(
            decide_rollout(&placed(Ring::Fleet, 0), &ramping, &rollout.revoked_keys),
            RolloutDecision::Install,
            "a bucket-0 default installs the moment the ramp leaves zero"
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
