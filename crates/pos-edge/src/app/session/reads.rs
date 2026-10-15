// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! What the edge reads from its session: the menu and price book for a channel, the tax table, the
//! capabilities, the tenders, the floor and the station a line is routed to, and the builders that
//! put each in place.

use std::collections::BTreeMap;

use pos_core::business_date::{CutoffHour, StoreTimeZone};
use pos_core::campaign::{Campaign, Connectivity};
use pos_core::capability::{Capability, CapabilityContext};
use pos_core::inventory::{RecipeBook, StockProjection};
use pos_core::permission::{Permission, PermissionSet};
use pos_proto::backup::PublishedBackup;
use pos_proto::counter::PublishedCounter;
use pos_proto::devices::PublishedDevices;
use pos_proto::display::DisplayPlan;
use pos_proto::fees::PublishedFees;
use pos_proto::floor::{FloorPlan, StationPlan};
use pos_proto::ids::{CourseId, FeeId, MenuItemId, StationId, TableId, TaxClassId};
use pos_proto::integrations::PublishedIntegrations;
use pos_proto::locale::{NumberFormat, TaxRate, TaxRateTable, TaxRounding};
use pos_proto::menu::{MenuBook, MenuCatalog, MenuEntry};
use pos_proto::money::CurrencyCode;
use pos_proto::printing::PublishedPrinting;
use pos_proto::qr::PublishedQr;
use pos_proto::reason_codes::PublishedReasonCodes;
use pos_proto::session::PublishedSession;
use pos_proto::shift::PublishedShift;
use pos_proto::store_profile::StoreProfile;
use pos_proto::tender_keys::PublishedTenderKeys;
use pos_proto::text::DisplayName;
use pos_proto::ulid::Ulid;
use pos_proto::{PaymentMethod, SalesChannel};

use crate::app::roster::StaffRoster;
use crate::app::session::EdgeSession;

impl EdgeSession {
    /// The well-known standard tax class the bootstrap rates the menu at, until the cloud config tree
    /// ([ADR-0004](../../../../../docs/adr/0004-cloud-owned-configuration.md)) supplies the real classes.
    /// A line drafted by the example or a test carries this class so its tax resolves.
    #[must_use]
    pub fn standard_tax_class() -> TaxClassId {
        TaxClassId::new(Ulid::from_u128(1))
    }

    /// The bootstrap tax table: the standard class at 10% on **every** channel a store can sell
    /// through.
    ///
    /// Every channel, not just dine-in, because roadmap **B1.2** made the rate a per-order lookup:
    /// a bill is now assembled at the rate of the channel *its own order* came in on, and
    /// [`billing::assemble`] refuses a class with no rate for that channel rather than papering
    /// over it with zero tax. A table carrying dine-in alone would therefore have stopped a
    /// LAN-only store from settling a takeaway order it had already accepted, priced and fired.
    ///
    /// Same 10% across the board, so a bootstrap store's arithmetic is unchanged by B1.2; the
    /// point is that each cell now *exists* and the cloud can override any one of them
    /// independently (M4's class × channel grid, [ADR-0074](../../../../../docs/adr/0074-localization-and-tax.md)).
    fn bootstrap_tax_rates() -> TaxRateTable {
        let standard = Self::standard_tax_class();
        let rate = TaxRate::from_percent(10);
        <SalesChannel as pos_proto::WireEnum>::ALL
            .iter()
            .copied()
            .filter(|channel| *channel != <SalesChannel as pos_proto::WireEnum>::UNSPECIFIED)
            .fold(TaxRateTable::new(), |table, channel| {
                table.with(standard, channel, rate)
            })
    }

    /// Bootstrap defaults until the cloud config tree ([ADR-0004](../../../../../docs/adr/0004-cloud-owned-configuration.md),
    /// P7) supplies the real values: a full-service store, every permission granted, VND, UTC with a
    /// 04:00 cut-off, offline. Enough for the edge to sell on fakes and for the dine-in flow to run.
    #[must_use]
    #[expect(
        clippy::missing_panics_doc,
        reason = "the only expect is on CutoffHour::new(4), a compile-time constant that is always valid"
    )]
    pub fn bootstrap() -> Self {
        Self {
            granted: Permission::ALL.iter().copied().collect(),
            capabilities: CapabilityContext::full_service(),
            published_flags: BTreeMap::new(),
            currency: CurrencyCode::VND,
            // Zero, because the bootstrap currency is the đồng and the đồng has no subunit. A
            // *named* fallback for a named currency, not a default standing in for an unknown one:
            // a store that syncs a locale node replaces both together.
            currency_exponent: 0,
            timezone: StoreTimeZone::utc(),
            cutoff: CutoffHour::new(4).expect("4 is a valid cut-off hour"),
            connectivity: Connectivity::Offline,
            recipes: RecipeBook::default(),
            tax_rates: Self::bootstrap_tax_rates(),
            // Exclusive until a locale publish says otherwise — the posture Vietnam trades on.
            prices_include_tax: false,
            // No cash rounding and no quick-cash keys until a locale publish supplies them: both are
            // country facts, and inventing either would put a wrong total or a wrong button on a
            // till that has not synced yet (ADR-0105).
            cash_rounding_increment: None,
            cash_denominations: Vec::new(),
            // Half-up until the `locale` node says otherwise, as every bill was before the setting.
            tax_rounding: TaxRounding::HalfUp,
            // A year, matching `LocalePack::default` — chosen rather than left unbounded, because
            // "forever until somebody publishes a locale" is the wrong default for personal data.
            retention_days: 365,
            event_log_days: 90,
            // The common convention — `1,234.5` — and not Vietnam's, deliberately: the bootstrap is
            // what an unsynced box of unknown country uses, and `pos-proto`'s own default test says
            // the same. A store that syncs a locale node replaces it.
            number_format: NumberFormat::default(),
            menu: MenuCatalog::new(),
            menu_book: MenuBook::new(),
            sales_channel: SalesChannel::DineIn,
            staff: StaffRoster::new(),
            permissions_enforced: false,
            floor: FloorPlan::new(),
            stations: StationPlan::new(),
            layout: DisplayPlan::new(),
            devices: PublishedDevices::default(),
            integrations: PublishedIntegrations::default(),
            // Nothing is invented here: a receipt headed with a guessed shop name is worse than one
            // headed with nothing (ADR-0106).
            profile: StoreProfile::default(),
            shift: PublishedShift::default(),
            session_settings: PublishedSession::default(),
            printing: PublishedPrinting::default(),
            qr: PublishedQr::default(),
            campaigns: Vec::new(),
            recipe_thresholds: BTreeMap::new(),
            enabled_channels: None,
            accepted_tender: None,
            tender_keys: PublishedTenderKeys::default(),
            counter: PublishedCounter::default(),
            backup: PublishedBackup::default(),
            display_language: None,
            country_language: None,
            qr_staff_confirmation_required: true,
            qr_enabled_published: None,
            reason_codes: PublishedReasonCodes::framework_default(),
            // No fee until a `fees` node is published: what every bill charged before it existed.
            fees: PublishedFees::default(),
            // No rollout published and no placement: a bootstrap store installs nothing, which is
            // the only safe answer before the cloud has said anything about updates.
            fleet_update: None,
            device_ota: None,
            // No lease issued: a bootstrap store is the only machine that has ever claimed it, so
            // there is nothing that could have superseded it.
            lease_generation: None,
        }
    }

    /// Authorises a staff sign-in against the published roster (ADR-0070): a correct `code` + `pin`
    /// yields that person's granted [`PermissionSet`]; anything else yields `None`. This is the store
    /// applying the cloud's published set rather than a local roster.
    #[must_use]
    pub fn authorise_staff(&self, code: &str, pin: &str) -> Option<PermissionSet> {
        self.staff.authorise(code, pin)
    }

    /// Installs a menu catalog, for a test or the on-fakes example. The real store's menu arrives
    /// from the cloud config tree (WS-B); this builder seeds one without a cloud.
    #[must_use]
    pub fn with_menu(mut self, menu: MenuCatalog) -> Self {
        self.menu = menu;
        self
    }

    /// Installs a price book, and the store's own channel's catalogue from it, as applying a
    /// published `menu` node does ([ADR-0066](../../../../../docs/adr/0066-cloud-catalog.md)). For a test
    /// or the example.
    #[must_use]
    pub fn with_menu_book(mut self, book: MenuBook) -> Self {
        self.menu = book.catalog_for(self.sales_channel).clone();
        self.menu_book = book;
        self
    }

    /// The catalogue an order on `channel` is priced from
    /// ([ADR-0066](../../../../../docs/adr/0066-cloud-catalog.md)).
    ///
    /// The channel's own price book when the published book prices that channel, by a row of its
    /// own or a fallback. Otherwise the store's own channel's catalogue, [`menu`](Self::menu): a
    /// channel nobody has priced keeps selling at the prices it always sold at, rather than
    /// refusing every dish on an upgrade.
    ///
    /// A channel the book does price is priced only from its own row, so a dish the cloud left off
    /// that channel, such as one not sold for delivery, is refused there.
    #[must_use]
    pub fn menu_for(&self, channel: SalesChannel) -> &MenuCatalog {
        let priced_here = self.menu_book.channels().iter().any(|row| {
            !row.sales_channel.is_unrecognised() && row.sales_channel.known() == channel
        });
        if channel == self.sales_channel || (!priced_here && self.menu_book.fallback().is_empty()) {
            &self.menu
        } else {
            self.menu_book.catalog_for(channel)
        }
    }

    /// The published entry for `menu_item_id` on any channel: the store's own channel first, then
    /// each channel the book prices.
    ///
    /// For a name, which a line needs whichever channel sold it. A dish sold only for takeaway is
    /// on no dine-in catalogue, and the kitchen board must still name it.
    #[must_use]
    pub fn menu_entry(&self, menu_item_id: MenuItemId) -> Option<&MenuEntry> {
        self.menu.get(menu_item_id).or_else(|| {
            self.menu_book
                .channels()
                .iter()
                .find_map(|row| row.catalog.get(menu_item_id))
                .or_else(|| self.menu_book.fallback().get(menu_item_id))
        })
    }

    /// The name the store's current rule for `fee_id` gives the fee in `language`, if it translates
    /// it ([ADR-0159](../../../../../docs/adr/0159-a-fee-is-configuration.md) decision 4).
    ///
    /// A bill freezes only the rule's own name, so paper or a screen in another language looks the
    /// translation up here, as a receipt does an item's in the menu, and keeps the frozen name where
    /// the rule has none, or is gone.
    #[must_use]
    pub fn fee_translation(&self, fee_id: FeeId, language: &str) -> Option<&DisplayName> {
        self.fees
            .fees
            .iter()
            .find(|rule| rule.fee_id == fee_id)
            .and_then(|rule| rule.display_name_translations.get(language))
    }

    /// Installs a channel-keyed tax table, for a test or the example.
    #[must_use]
    pub fn with_tax_rates(mut self, tax_rates: TaxRateTable) -> Self {
        self.tax_rates = tax_rates;
        self
    }

    /// Installs the store's campaigns, for a test or the example. The real store's promotions arrive
    /// from the cloud config tree's `campaigns` node ([ADR-0077](../../../../../docs/adr/0077-campaigns-and-scheduling.md));
    /// this builder seeds them without a cloud.
    #[must_use]
    pub fn with_campaigns(mut self, campaigns: Vec<Campaign>) -> Self {
        self.campaigns = campaigns;
        self
    }

    /// The auto-86 decision (§8) for `item` against a stock projection: whether strictly more than the
    /// item's authored threshold can be made from `stock`. An item with no recipe is always sellable;
    /// a tracked one uses its `recipe_thresholds` entry (defaulting to `0`).
    ///
    /// This is the pure decision the flagged goods-in/stocktake follow-up will drive with a live
    /// projection ([ADR-0079](../../../../../docs/adr/0079-inventory-and-suppliers.md)); the edge holds no
    /// on-hand stock yet, so the live menu is not 86'd from it on this slice — applying the recipes so a
    /// fired line consumes its bill of materials is the change M6 lands here.
    #[must_use]
    pub fn item_sellable(&self, item: MenuItemId, stock: &StockProjection) -> bool {
        let threshold = self.recipe_thresholds.get(&item).copied().unwrap_or(0);
        stock.available(item, &self.recipes).is_sellable(threshold)
    }

    /// Whether this store accepts orders on `channel` ([ADR-0080](../../../../../docs/adr/0080-channels-and-payments.md),
    /// M7). A store with no `channels` node published has no restriction (every channel enabled), so
    /// this is `true`; once a node is published only the channels it lists are accepted.
    #[must_use]
    pub fn channel_enabled(&self, channel: SalesChannel) -> bool {
        self.enabled_channels
            .as_ref()
            .is_none_or(|set| set.contains(&channel))
    }

    /// The value the store's published configuration sets for `capability`, or `None` where it sets
    /// none, which [`Self::capabilities`] reads as the flag's declared default.
    #[must_use]
    pub fn explicit(&self, capability: Capability) -> Option<bool> {
        self.published_flags.get(&capability).copied()
    }

    /// Whether the store's configuration switches QR ordering off, so that the intake refuses a QR
    /// order (ADR-0160 decision 5).
    ///
    /// Only the switch published `false` does, with `qr.enabled` `false` beside it, which is how a
    /// cloud that knows the switch always publishes it off. A configuration that does not carry the
    /// switch decides by the channel list alone ([`Self::channel_enabled`]), as the edge did before
    /// the switch, so an edge that updates before its cloud refuses nothing it took. Nor does a
    /// `false` beside a `qr.enabled` that is not `false`: that is the flag as it was before the
    /// switch, when the console's capability form published every flag, this one included, while
    /// nothing read it and the store took QR orders.
    #[must_use]
    pub fn qr_ordering_switched_off(&self) -> bool {
        self.explicit(Capability::QrOrdering) == Some(false)
            && self.qr_enabled_published == Some(false)
    }

    /// Whether this store accepts `method` as tender (ADR-0080, M7). Same opt-in rule as
    /// [`Self::channel_enabled`]: no `tender` node published means any known method is accepted.
    #[must_use]
    pub fn tender_accepted(&self, method: PaymentMethod) -> bool {
        self.accepted_tender
            .as_ref()
            .is_none_or(|set| set.contains(&method))
    }

    /// Whether a guest's order on `channel` naming `table_id` waits for a member of staff before
    /// the kitchen sees it ([ADR-0057](../../../../../docs/adr/0057-qr-ordering.md); the store can turn
    /// the hold off in its `qr` guardrail node,
    /// [ADR-0080](../../../../../docs/adr/0080-channels-and-payments.md) M7).
    ///
    /// **The single place this question is answered**, for the answer reported to the caller and
    /// for the answer that gates firing
    /// ([ADR-0116](../../../../../docs/adr/0116-the-qr-hold-is-derived-and-it-gates-firing.md)). It used
    /// to be asked twice — once to write the durable ledger row (as `table_id.is_some()` alone)
    /// and once to build the acceptance returned to the cloud (as
    /// `table_id.is_some() && qr_staff_confirmation_required`) — so on a store that had turned the
    /// hold off the two disagreed: the first delivery answered "not waiting" and every replay of
    /// the same at-least-once reference answered "waiting".
    ///
    /// The channel is an argument because this is now also asked of *every* order in the
    /// projection, not only of an order arriving through intake. On the intake path every tabled
    /// order is a QR order, so `table_id.is_some()` was right by accident; asked of the whole
    /// projection it would hold every dine-in table a server had seated by hand. A tabled order on
    /// the delivery channel is not a guest submission either, and is not held.
    ///
    /// Note what this is NOT: it is not "does this order get a queue number". That is
    /// `table_id.is_none()` — a table order is served where it sits, a tableless one is called back
    /// by number — and it holds however the hold is configured. Deriving one from the other is the
    /// mistake that made these two questions look like one.
    #[must_use]
    pub fn holds_for_staff_confirmation(
        &self,
        channel: Option<SalesChannel>,
        table_id: Option<TableId>,
    ) -> bool {
        channel == Some(SalesChannel::Qr)
            && table_id.is_some()
            && self.qr_staff_confirmation_required
    }

    /// Installs a capability profile, for a test or the on-fakes example. The real store's profile
    /// arrives from the cloud config tree's flag keys ([ADR-0071](../../../../../docs/adr/0071-config-without-json.md));
    /// this builder seeds one without a cloud.
    #[must_use]
    pub fn with_capabilities(mut self, capabilities: CapabilityContext) -> Self {
        self.capabilities = capabilities;
        self
    }

    /// Installs a staff roster, for a test or the on-fakes example. The real store's roster arrives
    /// from the cloud's `permissions` config node (ADR-0070); this builder seeds one without a cloud.
    #[must_use]
    pub fn with_staff(mut self, staff: StaffRoster) -> Self {
        self.staff = staff;
        self
    }

    /// Installs a floor plan, for a test or the on-fakes example. The real store's floor arrives from
    /// the cloud's `floor` config node ([ADR-0072](../../../../../docs/adr/0072-floor-and-kitchen.md)).
    #[must_use]
    pub fn with_floor(mut self, floor: FloorPlan) -> Self {
        self.floor = floor;
        self
    }

    /// Installs a station plan, for a test or the on-fakes example. The real store's kitchen arrives
    /// from the cloud's `stations` config node ([ADR-0072](../../../../../docs/adr/0072-floor-and-kitchen.md)).
    #[must_use]
    pub fn with_stations(mut self, stations: StationPlan) -> Self {
        self.stations = stations;
        self
    }

    /// The station a fired line routes to under the published plan (ADR-0072), or `None` when the
    /// store has no station plan or no rule matches and it names no default. A thin delegate to the
    /// pure `pos_core::floor::route_station`, so the edge derives the station from the published
    /// routing instead of trusting the caller.
    #[must_use]
    pub fn resolve_station(
        &self,
        menu_item_id: MenuItemId,
        course_id: Option<CourseId>,
    ) -> Option<StationId> {
        pos_core::floor::route_station(&self.stations, menu_item_id, course_id)
    }
}
