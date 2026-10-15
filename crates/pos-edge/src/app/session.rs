// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The session defaults a decision reads: the store's synced configuration, held by the edge.

use std::collections::{BTreeMap, BTreeSet};

use pos_core::business_date::{CutoffHour, StoreTimeZone};
use pos_core::campaign::{Campaign, Connectivity};
use pos_core::capability::{Capability, CapabilityContext};
use pos_core::inventory::RecipeBook;
use pos_core::lease::LeaseGeneration;
use pos_core::ota::{DeviceOtaAssignment, FleetRollout};
use pos_core::permission::PermissionSet;
use pos_proto::backup::PublishedBackup;
use pos_proto::counter::PublishedCounter;
use pos_proto::devices::PublishedDevices;
use pos_proto::display::DisplayPlan;
use pos_proto::fees::PublishedFees;
use pos_proto::floor::{FloorPlan, StationPlan};
use pos_proto::ids::MenuItemId;
use pos_proto::integrations::PublishedIntegrations;
use pos_proto::locale::{NumberFormat, TaxRateTable, TaxRounding};
use pos_proto::menu::{MenuBook, MenuCatalog};
use pos_proto::money::CurrencyCode;
use pos_proto::printing::PublishedPrinting;
use pos_proto::qr::PublishedQr;
use pos_proto::reason_codes::PublishedReasonCodes;
use pos_proto::session::PublishedSession;
use pos_proto::shift::PublishedShift;
use pos_proto::store_profile::StoreProfile;
use pos_proto::tender_keys::PublishedTenderKeys;
use pos_proto::{PaymentMethod, SalesChannel};

use crate::app::roster::StaffRoster;

mod reads;

/// The session defaults a decision reads — normally the store's synced configuration
/// ([ADR-0004](../../../../docs/adr/0004-cloud-owned-configuration.md)). Held here so the decision spine
/// is config-driven; the values arrive from the cloud config tree in P7.
#[derive(Debug, Clone)]
pub struct EdgeSession {
    /// What the acting role is granted (§9).
    pub granted: PermissionSet,
    /// The store's capability profile (§10).
    pub capabilities: CapabilityContext,
    /// The capability flags the store's published configuration sets, each with the value it sets
    /// ([ADR-0160](../../../../docs/adr/0160-everything-a-store-runs-differently-is-published-configuration.md)
    /// decision 5), read through [`Self::explicit`].
    ///
    /// [`Self::capabilities`] reads a flag the document leaves out as its declared default, which
    /// cannot tell a store that chose a value from one that never chose. This records only what the
    /// document says, so it is read from each document whole: one that sets no flag leaves it empty,
    /// even where the profile keeps its last value. At most one entry per flag in
    /// [`Capability::ALL`].
    pub published_flags: BTreeMap<Capability, bool>,
    /// The store's display language, from the `locale` node
    /// ([ADR-0074](../../../../docs/adr/0074-localization-and-tax.md)), or `None` when the store has
    /// set none.
    ///
    /// The menu is already resolved into this language at config-apply time, so nothing needed the
    /// code itself until a *second* list had to be localized — the reason codes ADR-0116's refusal
    /// picker offers, which the framework ships and a publish replaces, and which therefore cannot
    /// be pre-resolved the way a compiled menu is.
    pub display_language: Option<String>,
    /// The language of the store's country, as its `locale` node states it from the cloud's country
    /// pack (ADR-0105), or `None` where the node states none. What a receipt prints in under
    /// `RECEIPT_LANGUAGE_COUNTRY` (ADR-0160).
    pub country_language: Option<String>,
    /// The store's currency.
    pub currency: CurrencyCode,
    /// How many decimal places that currency has
    /// ([ADR-0134](../../../../docs/adr/0134-a-currency-says-how-many-decimals-it-has.md)).
    ///
    /// Beside `currency` because it is the same fact: an `amount_minor` means nothing without it.
    /// Published by the country pack and applied from the `locale` node, like every other value
    /// here — the till and the receipt both read it rather than each keeping a table.
    pub currency_exponent: u8,
    /// The store's timezone, for business-date derivation (ADR-0014).
    pub timezone: StoreTimeZone,
    /// The hour the trading day rolls over.
    pub cutoff: CutoffHour,
    /// Whether the store is currently online (drives the campaign engine's offline rule).
    pub connectivity: Connectivity,
    /// The recipes a fire consumes (§8). Empty in the bootstrap: an item with no recipe consumes
    /// nothing, so the flow runs before the menu's bill of materials is synced (P7).
    pub recipes: RecipeBook,
    /// The store's channel-keyed tax rates (D6). Vietnam v1 populates one class at one rate, which is
    /// a special case of this table, not a different model; carrying both dimensions from day one is
    /// what avoids a migration across every line ever written.
    pub tax_rates: TaxRateTable,
    /// Whether this store's menu prices already contain their tax
    /// ([ADR-0104](../../../../docs/adr/0104-multi-component-and-inclusive-tax.md)).
    ///
    /// `false` — the bootstrap default and Vietnam's posture — adds tax on top of the price. `true`
    /// is Japan's 税込 and India's MRP: the price is what the guest pays and the tax is extracted
    /// from inside it. Published on the `locale` node, so a store changes posture without a release.
    pub prices_include_tax: bool,
    /// What the grand total is rounded to in cash, in minor units, or `None` for no rounding
    /// ([ADR-0105](../../../../docs/adr/0105-a-country-pack-is-values.md)).
    ///
    /// A fact about the country's coinage: India rounds to the rupee because no smaller coin settles
    /// the difference, Vietnam to the thousand đồng because that is the smallest note, Japan not at
    /// all because the 1-yen coin circulates. `None` in the bootstrap, which is what the edge did
    /// unconditionally until the `locale` node could say otherwise.
    pub cash_rounding_increment: Option<i64>,
    /// How each tax amount and each fee rounds to the minor unit: the `locale` node's
    /// `tax_rounding`, a setting
    /// ([ADR-0160](../../../../docs/adr/0160-everything-a-store-runs-differently-is-published-configuration.md)
    /// decision 2), read through `LocaleSettings::tax_rounding`.
    ///
    /// Half-up in the bootstrap and wherever the node does not say otherwise, which is what every
    /// bill did before the setting existed. A bill is computed with the mode the session holds when
    /// it is computed; the cash rounding of its total is half-up whatever this says.
    pub tax_rounding: TaxRounding,
    /// How long the store keeps a personal record before the retention sweep scrubs it, in days —
    /// the `default_retention_days` a `LocalePack` has carried since
    /// [ADR-0027](../../../../docs/adr/0027-country-modules.md) and which, until
    /// [ADR-0107](../../../../docs/adr/0107-the-buyer-is-a-subject.md), no edge read.
    ///
    /// The clock the store's own subject store runs on: a B2B invoice's buyer is scrubbed this many
    /// days after the sale, whether or not the box has ever reached the cloud — which is the point,
    /// because a retention obligation that only discharges while the link is up is not one.
    /// Defaults to a year, the same figure `LocalePack::default` carries, so a store that has synced
    /// no locale still forgets.
    pub retention_days: u16,
    /// How many days the edge keeps a **synced** event before retention may delete it
    /// ([ADR-0145](../../../../docs/adr/0145-the-edge-keeps-events-until-synced-and-n-days-old.md)).
    ///
    /// Per store, from the cloud's `retention` node, within [`EVENT_LOG_DAYS`]; 90 until one
    /// arrives, the figure the documents have always promised. It is a floor, never a deadline: an
    /// event the link has not acknowledged, or one an open order still needs, stays whatever this
    /// says. Not [`Self::retention_days`], which is how long *personal data* lives, set by law.
    pub event_log_days: u16,
    /// How this store writes a number: its decimal mark, its group mark, and how many digits go in a
    /// group ([ADR-0136](../../../../docs/adr/0136-a-store-publishes-how-it-writes-numbers.md)).
    ///
    /// A convention of the place rather than of the money, which is why it arrives with the country
    /// and not with the currency: Vietnam writes `1.234.567,50` whether the figure is in đồng or in
    /// dollars. The till and the printed receipt each drew their own answer before this — `en-US` on
    /// the screen, a comma on the paper — so a Vietnamese store's own convention appeared nowhere.
    pub number_format: NumberFormat,
    /// The notes a guest hands over, ascending, in minor units — the till's quick-cash keys.
    ///
    /// Empty means "offer the exact amount only", which is the honest answer for a store whose
    /// country nobody has filled in: an unhelpful key on a till is worse than no key.
    pub cash_denominations: Vec<i64>,
    /// The store's authoritative menu — the price book an inbound `OrderIn` reprices from
    /// ([ADR-0063](../../../../docs/adr/0063-store-menu-catalog.md), [ADR-0064](../../../../docs/adr/0064-edge-order-in.md)).
    /// Empty in the bootstrap: a store accepts no inbound order until the cloud publishes its menu
    /// through the config tree (WS-B), which is a safe default — it never guesses a price.
    ///
    /// The catalogue for [`sales_channel`](Self::sales_channel). An order on another channel prices
    /// from [`menu_for`](Self::menu_for), which reads [`menu_book`](Self::menu_book).
    pub menu: MenuCatalog,
    /// Every channel's price book as the cloud compiled it
    /// ([ADR-0066](../../../../docs/adr/0066-cloud-catalog.md)), with names resolved to the store's
    /// language as `menu`'s are. Empty until a `menu` node is applied.
    pub menu_book: MenuBook,
    /// The channel a walk-in bill's order came in on, which selects the tax rate. `DineIn` for a
    /// full-service store; a marketplace order overrides it per bill (P11).
    pub sales_channel: SalesChannel,
    /// The store's staff and what each may do, as published from the cloud on the `permissions` config
    /// node ([ADR-0070](../../../../docs/adr/0070-people-and-access.md)). Empty in the bootstrap: a store
    /// authorises no one from a roster until the console publishes its people, the same
    /// safe-by-default shape as the menu.
    pub staff: StaffRoster,
    /// Whether the store decides every command with the signed-in person's own permissions rather
    /// than the store-wide [`Self::granted`]
    /// ([ADR-0158](../../../../docs/adr/0158-the-till-enforces-each-persons-own-permissions.md)
    /// decision 1 and Rollout), from the `permissions` node's `enforced`.
    ///
    /// `false` in the bootstrap and for every node published without it: the store-wide set, as
    /// before. `true`: a person holds what their roles grant, directly or with approval, and
    /// nothing else — a person the roster does not know holds nothing — and an approver must be
    /// somebody other than the person acting (decision 5).
    pub permissions_enforced: bool,
    /// The store's floor plan — its areas and tables, as published on the `floor` config node
    /// ([ADR-0072](../../../../docs/adr/0072-floor-and-kitchen.md)). Empty in the bootstrap: the store
    /// carries no roster of tables until the console publishes one (the in-store UI shows its own
    /// fallback until then).
    pub floor: FloorPlan,
    /// The store's kitchen plan — its stations and item→station routing, as published on the
    /// `stations` config node ([ADR-0072](../../../../docs/adr/0072-floor-and-kitchen.md)). Empty in the
    /// bootstrap; `resolve_station` returns `None` until a plan is published, so the caller keeps its
    /// own fallback.
    pub stations: StationPlan,
    /// How the till groups and orders its buttons, from the `layout` config node
    /// ([ADR-0066](../../../../docs/adr/0066-cloud-catalog.md)), resolved for this store's sales
    /// channel.
    ///
    /// The layout half of the catalog, deliberately apart from the price half: `menu` is what the
    /// domain reprices from and `layout` is what a screen draws, so a price change relays no buttons
    /// and a button moving reprices nothing. `pos_core` never reads this.
    ///
    /// Empty in the bootstrap, and an empty plan means "the console has laid nothing out", not "show
    /// nothing": the till falls back to the flat price book, which is what it drew before this
    /// existed (production-readiness **C4**).
    pub layout: DisplayPlan,
    /// The printers and kitchen displays this store may address, as published on the `devices`
    /// config node ([ADR-0100](../../../../docs/adr/0100-receipt-and-ticket-printing.md)).
    ///
    /// Empty in the bootstrap, and an empty node is an ordinary state rather than a fault: a
    /// LAN-only box that has never synced, and a shop with no printer at all, both look like this.
    /// What the dispatcher must never do is the thing the till did before this existed — claim
    /// "Printing receipt…" while nothing is wired.
    pub devices: PublishedDevices,
    /// The tenant's vendor connections that serve this store, from the published `integrations`
    /// node ([ADR-0153](../../../../docs/adr/0153-a-vendor-is-a-provider-the-cloud-chooses.md)).
    ///
    /// Empty in the bootstrap, and empty is the ordinary state: every family then runs its offline
    /// path — an invoice from the local range, a QR transfer confirmed by hand. It holds no secret
    /// and never could: a store never talks to an internet vendor itself, and the node has no field
    /// a credential could travel in.
    pub integrations: PublishedIntegrations,
    /// Who this store legally is, as the receipt prints it
    /// ([ADR-0106](../../../../docs/adr/0106-the-store-is-a-legal-person.md)): its registered name, its
    /// address, and the tax registration number a Japanese qualified invoice or an Indian tax
    /// invoice is not one without.
    ///
    /// Empty in the bootstrap, which is what every store had before the node existed — the receipt
    /// then starts at its number, as it always did.
    pub profile: StoreProfile,
    /// How the store runs its shifts, from the `shift` config node
    /// ([ADR-0160](../../../../docs/adr/0160-everything-a-store-runs-differently-is-published-configuration.md)).
    ///
    /// The defaults in the bootstrap, and every default is what the edge did before the node
    /// existed: a store sells with or without an open shift.
    pub shift: PublishedShift,
    /// How long a sign-in lasts on an idle device and how many wrong PINs lock a person out, from
    /// the `session` config node
    /// ([ADR-0160](../../../../docs/adr/0160-everything-a-store-runs-differently-is-published-configuration.md)).
    ///
    /// The defaults in the bootstrap, which are what the edge did before the node existed: five
    /// wrong PINs lock a person out for five minutes, and the sign-in window is the local file's
    /// or thirty minutes ([`crate::auth::Sessions`]).
    pub session_settings: PublishedSession,
    /// How the store prints the paper it hands a guest, from the `printing` config node (ADR-0160):
    /// the receipt's language and whether a settle prints it. The defaults in the bootstrap, which
    /// are what the edge did before the node existed.
    pub printing: PublishedPrinting,
    /// The settings on the `qr` config node (ADR-0160 item 2): what a guest's order does at a table
    /// that already has an open order. The defaults in the bootstrap, which are what the edge did
    /// before the node carried them.
    pub qr: PublishedQr,
    /// The store's authored promotions — the runtime `Campaign`s converted from the `campaigns`
    /// config node ([ADR-0077](../../../../docs/adr/0077-campaigns-and-scheduling.md)), which
    /// `pos_core::campaign::evaluate` prices a bill against. Empty in the bootstrap: a store runs no
    /// promotions until the console publishes them, the same safe default as the menu. (The node is
    /// delivered to the session here; wiring `evaluate` into the live bill flow is the flagged M3
    /// follow-up.)
    pub campaigns: Vec<Campaign>,
    /// The per-item auto-86 threshold (§8) from the `inventory` config node
    /// ([ADR-0079](../../../../docs/adr/0079-inventory-and-suppliers.md), M6): an item is 86'd at or below
    /// this many makeable. Empty in the bootstrap, and an item absent here defaults to `0` (86 only when
    /// nothing can be made). Paired with [`Self::recipes`]; [`Self::item_sellable`] reads both against a
    /// stock projection. The live projection that drives auto-86 arrives with the flagged
    /// goods-in/stocktake follow-up, so nothing 86s a trading store's menu on this slice alone.
    pub recipe_thresholds: BTreeMap<MenuItemId, i64>,
    /// The sales channels this store accepts, from the `channels` config node
    /// ([ADR-0080](../../../../docs/adr/0080-channels-and-payments.md), M7). `None` means no restriction —
    /// the bootstrap default and the behaviour of a store that has never published the node, so a
    /// channel is enabled unless a published node says otherwise. `Some(set)` is authoritative.
    pub enabled_channels: Option<BTreeSet<SalesChannel>>,
    /// The payment methods this store accepts, from the `tender` config node (ADR-0080, M7). `None`
    /// means no restriction (any known method), exactly as before M7; `Some(set)` is authoritative.
    pub accepted_tender: Option<BTreeSet<PaymentMethod>>,
    /// The keys the pay screen offers, from the `tender_keys` node
    /// ([ADR-0160](../../../../docs/adr/0160-everything-a-store-runs-differently-is-published-configuration.md)
    /// decision 2): its tip keys and how many guests its even split offers. The defaults, five, ten
    /// and fifteen percent and up to six guests, in the bootstrap and wherever the node sets none.
    pub tender_keys: PublishedTenderKeys,
    /// How the counter opens an order for a walk-in guest, from the `counter` node
    /// ([ADR-0160](../../../../docs/adr/0160-everything-a-store-runs-differently-is-published-configuration.md)
    /// decision 2): the channel it takes when the till names none. Takeaway, as every walk-in was
    /// before the setting, in the bootstrap and wherever the node sets none.
    pub counter: PublishedCounter,
    /// How often the store archives its database off the box, from the `backup` node
    /// ([ADR-0160](../../../../docs/adr/0160-everything-a-store-runs-differently-is-published-configuration.md)
    /// decision 6). Empty in the bootstrap and wherever the node sets none, which leaves the
    /// archive loop on this box's own interval ([`crate::backup_client`]).
    pub backup: PublishedBackup,
    /// Whether a QR order (one that names a table) waits for staff before the kitchen sees it — the
    /// `qr.staff_confirmation_required` guardrail ([ADR-0057], authored via ADR-0080's `qr` node, M7).
    /// Defaults to `true` (ADR-0057: a guest order waits unless the store turns confirmation off); the
    /// edge reads it from the published `qr` node so an operator can disable the hold.
    pub qr_staff_confirmation_required: bool,
    /// The `qr` node's `enabled` as the store's published configuration sets it, or `None` where it
    /// sets none. Not a switch: QR ordering's switch is the `qr_ordering_enabled` flag, and a cloud
    /// that knows the switch publishes `enabled` equal to it (ADR-0160 decision 5). Read only to
    /// tell the switch published off from the flag's `false` from before the switch
    /// ([`Self::qr_ordering_switched_off`]).
    pub qr_enabled_published: Option<bool>,
    /// The managed reason-code list this store validates a rejection against
    /// ([ADR-0115](../../../../docs/adr/0115-reason-codes-are-a-managed-list.md)).
    ///
    /// Starts as [`PublishedReasonCodes::framework_default`] and not as an empty list, which is the
    /// one node where absence must not mean "feature off": a store that has published nothing must
    /// still be able to refuse a guest's order, and an empty list would make the picker empty and
    /// the refusal impossible. A published `reason_codes` node replaces this set wholesale
    /// (ADR-0115 slice 4).
    pub reason_codes: PublishedReasonCodes,
    /// The fee rules a bill may apply, from the `fees` config node
    /// ([ADR-0159](../../../../docs/adr/0159-a-fee-is-configuration.md) decision 1).
    ///
    /// Empty in the bootstrap and whenever the node is absent, and empty charges no fee, which is
    /// what every bill charged before the node existed. A bill does not read this as it goes: it
    /// freezes the rules in force on its channel when it opens and is computed from those
    /// (decision 3), so a publish during a meal reaches the next bill, not the one on the table.
    pub fees: PublishedFees,
    /// The rollout the cloud has published for the fleet, from the `fleet_update` config node
    /// ([ADR-0048](../../../../docs/adr/0048-ota-rollout-model.md)): the update every device weighs
    /// itself against — target version, lowest eligible ring, canary ramp, signing key, kill switch
    /// — together with the signing keys revocation has retired
    /// ([ADR-0047](../../../../docs/adr/0047-minisign-verification.md)).
    ///
    /// Held as one value rather than as an update beside a key list, because the node validates as
    /// one: `FleetUpdateConfig::validate` either yields both halves or neither, and splitting them
    /// here would let a later edit apply a rollout while leaving yesterday's revocations in place.
    /// Revocation is the half that must travel through configuration rather than being baked in
    /// beside the trusted keys — a compromised key has to stop being accepted without shipping a
    /// release that the compromised key itself would sign, and a revoked key is refused even when
    /// its signature verifies.
    ///
    /// `None` in the bootstrap and whenever the node is absent: no rollout is published, so there is
    /// nothing to install. An unparseable node leaves the previous value, as every other node here
    /// does; a rollout the console has since stopped is expressed by `halted` inside the node rather
    /// than by removing it, so halting never depends on a delete arriving.
    ///
    /// The cloud has published this node since P9e-3; **nothing on the edge read it until now**,
    /// which is why the OTA path was dark at the rollout decision as well as at the fetch.
    pub fleet_update: Option<FleetRollout>,
    /// Where the cloud has placed *this* device in the rollout, from the `device_ota` config node
    /// ([ADR-0048](../../../../docs/adr/0048-ota-rollout-model.md)): its ring and its stable canary
    /// bucket.
    ///
    /// `None` means the cloud has not placed this device, and an unplaced device is eligible for
    /// **no** rollout. That is deliberately an `Option` rather than a field with a default, because
    /// every plausible default is wrong in the dangerous direction. `Ring::Lab` reads like the
    /// cautious choice and is the *least* cautious one available: lab is the first ring a rollout
    /// opens to, and `decide_rollout` exempts it from the canary ramp entirely, so a device
    /// defaulted to Lab installs at the stage where an update has been proven on nothing.
    /// `Ring::Fleet` with bucket 0 is barely better — it is the first fleet device in, at any ramp
    /// above zero. There is no safe placement to invent, so none is invented.
    pub device_ota: Option<DeviceOtaAssignment>,
    /// The store's **authoritative** lease generation, from the `lease` config node
    /// ([ADR-0108](../../../../docs/adr/0108-the-lease-generation-is-authority.md)) — the number this
    /// box's own held generation is compared against to decide whether it is still the store.
    ///
    /// `None` means the cloud has never issued this store a lease, which is every store until an
    /// operator deliberately does. A box then weighs itself as active, exactly as the whole fleet
    /// did before this node existed — the refusal begins the day a store is first given a lease,
    /// not the day this field shipped.
    ///
    /// Deliberately *not* the generation the box holds: that one is durable edge-local state
    /// ([`crate::lease_state`]), because a value rebuilt from config on every boot would be
    /// re-adopted on every boot, and a superseded machine would promote itself back.
    pub lease_generation: Option<LeaseGeneration>,
}
