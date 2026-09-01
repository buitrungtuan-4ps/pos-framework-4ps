// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The application layer: load → decide → apply → publish.
//!
//! This is the thin layer [ADR-0013](../../../docs/adr/0013-async-strategy.md) says each binary
//! owns. For every command it: **loads** the aggregate's current state from the projection,
//! **decides** with the synchronous `pos-core` spine ([`decide_table`] and its siblings), **applies**
//! the decision by writing the wire events it maps to inside one store transaction, and — only after
//! the commit — **publishes** the change to every device over the fan-out and folds it into the
//! projection. A rolled-back transaction therefore prints nothing, tells no device, and changes no
//! screen.
//!
//! [`Edge`] is generic over the store `S`, so the same loop runs against `pos-fakes` in a test and
//! `store-sqlite` on a real machine — static dispatch, no `dyn`
//! ([ADR-0013](../../../docs/adr/0013-async-strategy.md)). It wires the **table floor cycle** (seat,
//! clean) and the **order line** (add, fire); the bill and shift families follow the identical shape.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::num::NonZeroU32;
use std::sync::{Arc, Mutex, RwLock};

use pos_core::billing::{self, BillInput, ClassBase, Payment};
use pos_core::business_date::{CutoffHour, StoreTimeZone, derive_business_date};
use pos_core::campaign::{Campaign, Connectivity};
use pos_core::capability::CapabilityContext;
use pos_core::decision::{
    Actor, BillCommand, DecisionCtx, Effect, LineCommand, ShiftCommand, TableCommand, decide_bill,
    decide_line, decide_shift, decide_table,
};
use pos_core::error::DomainError;
use pos_core::inventory::{RecipeBook, StockProjection};
use pos_core::menu::PricedLine;
use pos_core::permission::{Permission, PermissionSet};
use pos_ports::event_store::{EventQuery, EventStore};
use pos_ports::intake_ledger::{IntakeLedger, IntakeRecord};
use pos_ports::{PortError, TxContext};
use pos_proto::envelope::{DecodeError, EventEnvelope, EventPayload, EventTypeRef, RawPayload};
use pos_proto::events::{
    BillingBillOpened, BillingBillSettled, BillingPaymentCaptured, CashShiftClosed,
    CashShiftCounted, CashShiftOpened, DeviceActivationCompleted, EventType, KitchenTicketBumped,
    SalesOrderLineAdded, SalesOrderLineFired, SalesOrderOpened, SalesTableClosed, SalesTableOpened,
};
use pos_proto::floor::{FloorPlan, StationPlan};
use pos_proto::ids::{
    BillId, BrandId, CourseId, DeviceId, EmployeeId, MenuItemId, OrderId, OrderLineId, PaymentId,
    ShiftId, StationId, StoreId, TableId, TaxClassId, TenantId,
};
use pos_proto::locale::{TaxRate, TaxRateTable};
use pos_proto::menu::MenuCatalog;
use pos_proto::money::{CurrencyCode, Money, Ratio, Rounding};
use pos_proto::quantity::Quantity;
use pos_proto::text::DisplayName;
use pos_proto::time::{BusinessDate, Timestamp};
use pos_proto::ulid::Ulid;
use pos_proto::{
    BillState, ClockSource, IdGenerator, Open, OrderLineState, PaymentMethod, PaymentOutcome,
    SalesChannel, ShiftState, TableState,
};

use crate::clock::SystemClock;
use crate::fanout::{Fanout, ServerMessage};
use crate::idgen::EdgeIdGenerator;
use crate::receipt::ReceiptAuthority;

/// Which tenant, brand and store this edge is — the envelope context every event carries. Assigned
/// at activation ([ADR-0003](../../../docs/adr/0003-cattle-not-pets.md)); all three are identifiers,
/// not PII.
#[derive(Debug, Clone, Copy)]
pub struct StoreIdentity {
    /// Owning tenant.
    pub tenant_id: TenantId,
    /// Owning brand.
    pub brand_id: BrandId,
    /// This store.
    pub store_id: StoreId,
}

impl StoreIdentity {
    /// The identity for a store, with bootstrap tenant and brand ids until activation
    /// ([ADR-0003](../../../docs/adr/0003-cattle-not-pets.md)) supplies the real ones.
    #[must_use]
    pub fn for_store(store_id: StoreId) -> Self {
        Self {
            tenant_id: TenantId::new(Ulid::from_u128(1)),
            brand_id: BrandId::new(Ulid::from_u128(1)),
            store_id,
        }
    }
}

/// One staff member as the store authorises them ([ADR-0070](../../../docs/adr/0070-people-and-access.md)):
/// the permission set their assigned role grants, and the Argon2id PIN hash the edge verifies against
/// offline ([ADR-0030](../../../docs/adr/0030-pairing-and-offline-auth.md)). Both arrive in the store's
/// published `permissions` config node — the store never invents them.
#[derive(Debug, Clone)]
pub struct StaffAuth {
    /// The employee this badge code signs in as — the identity a command runs under once they sign in
    /// (S0b, [ADR-0084](../../../docs/adr/0084-device-authentication.md)). `None` if the published node
    /// carried no (or a malformed) id, in which case the member cannot sign in — the edge never invents
    /// an identity to act as.
    pub employee_id: Option<EmployeeId>,
    /// What the person's role grants (§9).
    pub permissions: PermissionSet,
    /// The Argon2id PHC hash of their PIN, or `None` if none is set (they cannot sign in until one is).
    pub pin_phc: Option<String>,
}

/// The store's staff, keyed by the badge `code` a person types, as published from the cloud
/// ([ADR-0070](../../../docs/adr/0070-people-and-access.md)). This is the roster the edge authorises
/// against — replacing any local, out-of-band staff list with the set the console published.
#[derive(Debug, Clone, Default)]
pub struct StaffRoster {
    by_code: BTreeMap<String, StaffAuth>,
}

impl StaffRoster {
    /// An empty roster — the bootstrap default until the cloud publishes the `permissions` node.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Adds (or replaces) a staff member under their `code`.
    pub fn insert(&mut self, code: impl Into<String>, auth: StaffAuth) {
        self.by_code.insert(code.into(), auth);
    }

    /// The staff member under `code`, if any.
    #[must_use]
    pub fn get(&self, code: &str) -> Option<&StaffAuth> {
        self.by_code.get(code)
    }

    /// How many staff the roster holds.
    #[must_use]
    pub fn len(&self) -> usize {
        self.by_code.len()
    }

    /// Whether the roster is empty (no staff published yet).
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.by_code.is_empty()
    }

    /// Authorises a sign-in: verifies `pin` against the published Argon2id hash for `code`, returning
    /// the granted [`PermissionSet`] on success. `None` for an unknown code, a member with no PIN set,
    /// or a wrong PIN — so a bad sign-in never yields any permissions. The rate-limit that turns
    /// repeated failures into a lockout is [`crate::auth::Lockout`] (ADR-0030); this is the pure
    /// verify against the published set.
    #[must_use]
    pub fn authorise(&self, code: &str, pin: &str) -> Option<PermissionSet> {
        let auth = self.by_code.get(code)?;
        let phc = auth.pin_phc.as_deref()?;
        crate::auth::verify_pin(phc, pin).then_some(auth.permissions)
    }

    /// The sign-in credentials under `code`: the employee the code acts as and the Argon2id hash to
    /// verify the PIN against. `None` for an unknown code, a member with no id, or one with no PIN set
    /// — the three cases a sign-in cannot proceed from. The rate-limited verification itself is
    /// [`crate::auth::Lockout::authenticate`] (ADR-0030); this only resolves what to verify against, so
    /// the HTTP sign-in route holds no roster detail (S0b, ADR-0084).
    #[must_use]
    pub fn credentials(&self, code: &str) -> Option<(EmployeeId, &str)> {
        let auth = self.by_code.get(code)?;
        Some((auth.employee_id?, auth.pin_phc.as_deref()?))
    }
}

/// The session defaults a decision reads — normally the store's synced configuration
/// ([ADR-0004](../../../docs/adr/0004-cloud-owned-configuration.md)). Held here so the decision spine
/// is config-driven; the values arrive from the cloud config tree in P7.
#[derive(Debug, Clone)]
pub struct EdgeSession {
    /// What the acting role is granted (§9).
    pub granted: PermissionSet,
    /// The store's capability profile (§10).
    pub capabilities: CapabilityContext,
    /// The store's currency.
    pub currency: CurrencyCode,
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
    /// The store's authoritative menu — the price book an inbound `OrderIn` reprices from
    /// ([ADR-0063](../../../docs/adr/0063-store-menu-catalog.md), [ADR-0064](../../../docs/adr/0064-edge-order-in.md)).
    /// Empty in the bootstrap: a store accepts no inbound order until the cloud publishes its menu
    /// through the config tree (WS-B), which is a safe default — it never guesses a price.
    pub menu: MenuCatalog,
    /// The channel a walk-in bill's order came in on, which selects the tax rate. `DineIn` for a
    /// full-service store; a marketplace order overrides it per bill (P11).
    pub sales_channel: SalesChannel,
    /// The store's staff and what each may do, as published from the cloud on the `permissions` config
    /// node ([ADR-0070](../../../docs/adr/0070-people-and-access.md)). Empty in the bootstrap: a store
    /// authorises no one from a roster until the console publishes its people, the same
    /// safe-by-default shape as the menu.
    pub staff: StaffRoster,
    /// The store's floor plan — its areas and tables, as published on the `floor` config node
    /// ([ADR-0072](../../../docs/adr/0072-floor-and-kitchen.md)). Empty in the bootstrap: the store
    /// carries no roster of tables until the console publishes one (the in-store UI shows its own
    /// fallback until then).
    pub floor: FloorPlan,
    /// The store's kitchen plan — its stations and item→station routing, as published on the
    /// `stations` config node ([ADR-0072](../../../docs/adr/0072-floor-and-kitchen.md)). Empty in the
    /// bootstrap; `resolve_station` returns `None` until a plan is published, so the caller keeps its
    /// own fallback.
    pub stations: StationPlan,
    /// The store's authored promotions — the runtime `Campaign`s converted from the `campaigns`
    /// config node ([ADR-0077](../../../docs/adr/0077-campaigns-and-scheduling.md)), which
    /// `pos_core::campaign::evaluate` prices a bill against. Empty in the bootstrap: a store runs no
    /// promotions until the console publishes them, the same safe default as the menu. (The node is
    /// delivered to the session here; wiring `evaluate` into the live bill flow is the flagged M3
    /// follow-up.)
    pub campaigns: Vec<Campaign>,
    /// The per-item auto-86 threshold (§8) from the `inventory` config node
    /// ([ADR-0079](../../../docs/adr/0079-inventory-and-suppliers.md), M6): an item is 86'd at or below
    /// this many makeable. Empty in the bootstrap, and an item absent here defaults to `0` (86 only when
    /// nothing can be made). Paired with [`Self::recipes`]; [`Self::item_sellable`] reads both against a
    /// stock projection. The live projection that drives auto-86 arrives with the flagged
    /// goods-in/stocktake follow-up, so nothing 86s a trading store's menu on this slice alone.
    pub recipe_thresholds: BTreeMap<MenuItemId, i64>,
    /// The sales channels this store accepts, from the `channels` config node
    /// ([ADR-0080](../../../docs/adr/0080-channels-and-payments.md), M7). `None` means no restriction —
    /// the bootstrap default and the behaviour of a store that has never published the node, so a
    /// channel is enabled unless a published node says otherwise. `Some(set)` is authoritative.
    pub enabled_channels: Option<BTreeSet<SalesChannel>>,
    /// The payment methods this store accepts, from the `tender` config node (ADR-0080, M7). `None`
    /// means no restriction (any known method), exactly as before M7; `Some(set)` is authoritative.
    pub accepted_tender: Option<BTreeSet<PaymentMethod>>,
    /// Whether a QR order (one that names a table) waits for staff before the kitchen sees it — the
    /// `qr.staff_confirmation_required` guardrail ([ADR-0057], authored via ADR-0080's `qr` node, M7).
    /// Defaults to `true` (ADR-0057: a guest order waits unless the store turns confirmation off); the
    /// edge reads it from the published `qr` node so an operator can disable the hold.
    pub qr_staff_confirmation_required: bool,
}

impl EdgeSession {
    /// The well-known standard tax class the bootstrap rates the menu at, until the cloud config tree
    /// ([ADR-0004](../../../docs/adr/0004-cloud-owned-configuration.md)) supplies the real classes.
    /// A line drafted by the example or a test carries this class so its tax resolves.
    #[must_use]
    pub fn standard_tax_class() -> TaxClassId {
        TaxClassId::new(Ulid::from_u128(1))
    }

    /// Bootstrap defaults until the cloud config tree ([ADR-0004](../../../docs/adr/0004-cloud-owned-configuration.md),
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
            currency: CurrencyCode::VND,
            timezone: StoreTimeZone::utc(),
            cutoff: CutoffHour::new(4).expect("4 is a valid cut-off hour"),
            connectivity: Connectivity::Offline,
            recipes: RecipeBook::default(),
            tax_rates: TaxRateTable::new().with(
                Self::standard_tax_class(),
                SalesChannel::DineIn,
                TaxRate::from_percent(10),
            ),
            menu: MenuCatalog::new(),
            sales_channel: SalesChannel::DineIn,
            staff: StaffRoster::new(),
            floor: FloorPlan::new(),
            stations: StationPlan::new(),
            campaigns: Vec::new(),
            recipe_thresholds: BTreeMap::new(),
            enabled_channels: None,
            accepted_tender: None,
            qr_staff_confirmation_required: true,
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

    /// Installs a channel-keyed tax table, for a test or the example.
    #[must_use]
    pub fn with_tax_rates(mut self, tax_rates: TaxRateTable) -> Self {
        self.tax_rates = tax_rates;
        self
    }

    /// Installs the store's campaigns, for a test or the example. The real store's promotions arrive
    /// from the cloud config tree's `campaigns` node ([ADR-0077](../../../docs/adr/0077-campaigns-and-scheduling.md));
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
    /// projection ([ADR-0079](../../../docs/adr/0079-inventory-and-suppliers.md)); the edge holds no
    /// on-hand stock yet, so the live menu is not 86'd from it on this slice — applying the recipes so a
    /// fired line consumes its bill of materials is the change M6 lands here.
    #[must_use]
    pub fn item_sellable(&self, item: MenuItemId, stock: &StockProjection) -> bool {
        let threshold = self.recipe_thresholds.get(&item).copied().unwrap_or(0);
        stock.available(item, &self.recipes).is_sellable(threshold)
    }

    /// Whether this store accepts orders on `channel` ([ADR-0080](../../../docs/adr/0080-channels-and-payments.md),
    /// M7). A store with no `channels` node published has no restriction (every channel enabled), so
    /// this is `true`; once a node is published only the channels it lists are accepted.
    #[must_use]
    pub fn channel_enabled(&self, channel: SalesChannel) -> bool {
        self.enabled_channels
            .as_ref()
            .is_none_or(|set| set.contains(&channel))
    }

    /// Whether this store accepts `method` as tender (ADR-0080, M7). Same opt-in rule as
    /// [`Self::channel_enabled`]: no `tender` node published means any known method is accepted.
    #[must_use]
    pub fn tender_accepted(&self, method: PaymentMethod) -> bool {
        self.accepted_tender
            .as_ref()
            .is_none_or(|set| set.contains(&method))
    }

    /// Installs a capability profile, for a test or the on-fakes example. The real store's profile
    /// arrives from the cloud config tree's flag keys ([ADR-0071](../../../docs/adr/0071-config-without-json.md));
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
    /// the cloud's `floor` config node ([ADR-0072](../../../docs/adr/0072-floor-and-kitchen.md)).
    #[must_use]
    pub fn with_floor(mut self, floor: FloorPlan) -> Self {
        self.floor = floor;
        self
    }

    /// Installs a station plan, for a test or the on-fakes example. The real store's kitchen arrives
    /// from the cloud's `stations` config node ([ADR-0072](../../../docs/adr/0072-floor-and-kitchen.md)).
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

/// A failure applying a command.
#[derive(Debug, thiserror::Error)]
pub enum AppError {
    /// The domain refused the command — a bad transition, a missing permission, a disabled capability.
    #[error("the command was refused: {0}")]
    Domain(#[from] DomainError),
    /// The store could not be read or written.
    #[error("the store failed: {0}")]
    Port(#[from] PortError),
    /// The business date could not be derived from the clock and the store's timezone.
    #[error("could not derive the business date")]
    Clock,
    /// An event payload could not be encoded to JSON.
    #[error("could not encode an event: {0}")]
    Encode(DecodeError),
    /// A line was added to a table that has no open order — it must be seated first.
    #[error("the table has no open order")]
    NoOpenOrder,
    /// A command named a line the edge does not know.
    #[error("no such order line")]
    UnknownLine,
    /// A line was fired but no station could be resolved — the store has no station plan (no rule
    /// matched and none is the default) and the caller named no station either (ADR-0072).
    #[error("the line routes to no station")]
    UnroutableLine,
    /// A command named a bill the edge does not know.
    #[error("no such bill")]
    UnknownBill,
    /// A command named a shift the edge does not know.
    #[error("no such shift")]
    UnknownShift,
    /// A shift was opened while one is already open — one open shift per device (§6, archive).
    #[error("a shift is already open")]
    ShiftAlreadyOpen,
}

/// What a table looks like to a caller after a command.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TableView {
    /// The table.
    pub table_id: TableId,
    /// Its state after the command.
    pub state: TableState,
}

/// A line as the caller wants it added — the amounts captured from the menu the device holds
/// (`sales.order_line.added` §: a line never references the live menu, so these are recorded now and
/// never looked up later). The edge does not invent prices; it records what the device supplies.
#[derive(Debug, Clone)]
pub struct LineDraft {
    /// The menu item, for reporting.
    pub menu_item_id: MenuItemId,
    /// The name shown to the guest at this moment.
    pub display_name: DisplayName,
    /// How many.
    pub quantity: Quantity,
    /// Unit price at this moment.
    pub unit_price: Money,
    /// Extended total at this moment.
    pub line_total: Money,
    /// Tax class at this moment.
    pub tax_class_id: TaxClassId,
    /// Tax rate in force at this moment.
    pub tax_rate: Ratio,
    /// Seat, when seats are enabled.
    pub seat: Option<u16>,
    /// Course, when courses are enabled.
    pub course_id: Option<CourseId>,
    /// Whether a guest note was written — its text never enters the log.
    pub note_present: bool,
}

/// What a line looks like to a caller after a command.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LineView {
    /// The order the line belongs to.
    pub order_id: OrderId,
    /// The line.
    pub order_line_id: OrderLineId,
    /// Its state after the command.
    pub state: OrderLineState,
}

/// What a KDS bump looks like to a caller: the order and station, and the lines now marked prepared.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BumpView {
    /// The order whose ticket was bumped.
    pub order_id: OrderId,
    /// The station that bumped it.
    pub station_id: StationId,
    /// The lines now marked prepared.
    pub order_line_ids: Vec<OrderLineId>,
}

/// What a bill looks like to a caller after a command.
///
/// After a settle it carries the gapless receipt number and the total the guest paid, plus the
/// state the bill and its table moved to. `print_receipt` reflects the [`Effect::PrintReceipt`]
/// the domain returned: the edge does not itself hold a printer, so the caller (the binary) runs the
/// effect after this returns — a rolled-back settle therefore never prints.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BillView {
    /// The bill.
    pub bill_id: BillId,
    /// Its state after the command.
    pub state: BillState,
    /// The gapless per-store receipt number, present once the bill has settled. Never a legal
    /// invoice number (ADR-0025).
    pub receipt_number: Option<u64>,
    /// What the guest owed, present once settled.
    pub total_due: Option<Money>,
    /// The state the bill's table moved to as a result (P5 derives the floor cycle from the bill).
    pub table_state: TableState,
    /// Whether the settle asked for a receipt to be printed, for the caller to run after commit.
    pub print_receipt: bool,
}

/// What a cash shift looks like to a caller after a command.
///
/// The close is **blind** (§11.1): counting reveals nothing, so `expected_amount` and `variance` are
/// `None` on an open or counted shift and are populated only once the shift closes — the cashier
/// counts before the system says what it expected.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ShiftView {
    /// The shift.
    pub shift_id: ShiftId,
    /// Its state after the command.
    pub state: ShiftState,
    /// What the system expected in the drawer, revealed only at close.
    pub expected_amount: Option<Money>,
    /// What the cashier counted, once counted.
    pub counted_amount: Option<Money>,
    /// Counted minus expected, revealed only at close. Negative means short.
    pub variance: Option<Money>,
    /// Whether closing asked for the shift report to print, for the caller to run after commit.
    pub print_shift_report: bool,
}

/// What the projection remembers about one order line, so a fire can be decided and its consumption
/// computed, and a bill assembled, without re-reading the event log.
#[derive(Debug, Clone, Copy)]
struct LineRecord {
    order_id: OrderId,
    state: OrderLineState,
    menu_item_id: MenuItemId,
    quantity: Quantity,
    course_id: Option<CourseId>,
    /// The extended total the device captured at add time — the base a bill sums per tax class. A
    /// line never re-reads the live menu (§14.2), so this is authoritative.
    line_total: Money,
    /// The tax class captured at add time, which keys the line into a rate on the bill.
    tax_class_id: TaxClassId,
}

/// What the projection remembers about one bill: the order it bills, the table that order sits on
/// (so settling can cycle the table), and its state (so a second settle is refused).
#[derive(Debug, Clone, Copy)]
struct BillRecord {
    order_id: OrderId,
    table_id: TableId,
    state: BillState,
}

/// What the projection remembers about a cash shift: its state, the float it opened with, the cash
/// its settled bills have taken (the rollup the close compares against), and the blind count once
/// entered. The expected drawer amount is `opening_float + cash_collected`; keeping the count here
/// but never revealing the expectation before close is what makes the close blind (§11.1).
#[derive(Debug, Clone, Copy)]
struct ShiftRecord {
    state: ShiftState,
    opening_float: Money,
    cash_collected: Money,
    counted: Option<Money>,
}

impl ShiftRecord {
    /// The cash the drawer is expected to hold: the float plus every cash payment taken on the
    /// shift. Cash rounding and non-cash tenders never touch the drawer, so they are not in it.
    fn expected(&self) -> Result<Money, DomainError> {
        Ok(self.opening_float.checked_add(self.cash_collected)?)
    }
}

/// The in-memory projection: the current state of each aggregate, folded from the event log.
///
/// Durable truth is the event log in the store; this is a cache rebuilt at boot and updated as
/// decisions apply, so a screen is answered without replaying events per request. This slice tracks
/// tables, the order each table holds, and each line's state; the bill and shift aggregates join it
/// as their flows land.
#[derive(Debug, Default)]
struct Projection {
    tables: HashMap<TableId, TableState>,
    table_orders: HashMap<TableId, OrderId>,
    lines: HashMap<OrderLineId, LineRecord>,
    bills: HashMap<BillId, BillRecord>,
    shifts: HashMap<ShiftId, ShiftRecord>,
    /// The one shift currently trading or counted, if any — the drawer cash lands on it, and every
    /// event minted while it is set carries its id. Cleared when the shift closes.
    open_shift: Option<ShiftId>,
    /// The lines a station has bumped (marked prepared, `kitchen.ticket.bumped`). Tracked apart from
    /// [`OrderLineState`] because a bump is orthogonal to the line's order state (a fired line is
    /// still "fired" once made); this is what lets a second KDS agree that a ticket is done.
    bumped_lines: HashSet<OrderLineId>,
}

impl Projection {
    fn table_state(&self, table_id: TableId) -> TableState {
        self.tables
            .get(&table_id)
            .copied()
            .unwrap_or(TableState::Free)
    }

    fn set_table(&mut self, table_id: TableId, state: TableState) {
        self.tables.insert(table_id, state);
    }

    fn open_order(&mut self, table_id: TableId, order_id: OrderId) {
        self.table_orders.insert(table_id, order_id);
    }

    fn order_for_table(&self, table_id: TableId) -> Option<OrderId> {
        self.table_orders.get(&table_id).copied()
    }

    /// The table an order sits on, if any — the reverse of [`Self::open_order`], used by rebuild to
    /// recover a bill's table from the log (a bill event names its order, not its table).
    fn table_for_order(&self, order_id: OrderId) -> Option<TableId> {
        self.table_orders
            .iter()
            .find(|(_, order)| **order == order_id)
            .map(|(table, _)| *table)
    }

    fn add_line(&mut self, line_id: OrderLineId, record: LineRecord) {
        self.lines.insert(line_id, record);
    }

    fn line(&self, line_id: OrderLineId) -> Option<LineRecord> {
        self.lines.get(&line_id).copied()
    }

    fn set_line_state(&mut self, line_id: OrderLineId, state: OrderLineState) {
        if let Some(record) = self.lines.get_mut(&line_id) {
            record.state = state;
        }
    }

    /// Marks lines bumped (prepared) — from a live `bump_ticket` and from the fold on rebuild.
    fn mark_bumped(&mut self, order_line_ids: &[OrderLineId]) {
        self.bumped_lines.extend(order_line_ids.iter().copied());
    }

    /// Every bumped line, in id order — the current prepared set a KDS reads on (re)connect so a
    /// screen that joined late agrees with the ones that were already open.
    fn bumped_line_ids(&self) -> Vec<OrderLineId> {
        let mut ids: Vec<OrderLineId> = self.bumped_lines.iter().copied().collect();
        ids.sort_unstable();
        ids
    }

    /// The lines on an order, in id order so the assembly (and its rounding residual) is
    /// deterministic across runs regardless of the hash map's iteration order.
    fn lines_for_order(&self, order_id: OrderId) -> Vec<LineRecord> {
        let mut lines: Vec<(OrderLineId, LineRecord)> = self
            .lines
            .iter()
            .filter(|(_, record)| record.order_id == order_id)
            .map(|(id, record)| (*id, *record))
            .collect();
        lines.sort_by_key(|(id, _)| *id);
        lines.into_iter().map(|(_, record)| record).collect()
    }

    fn open_bill_record(&mut self, bill_id: BillId, record: BillRecord) {
        self.bills.insert(bill_id, record);
    }

    fn bill(&self, bill_id: BillId) -> Option<BillRecord> {
        self.bills.get(&bill_id).copied()
    }

    fn set_bill_state(&mut self, bill_id: BillId, state: BillState) {
        if let Some(record) = self.bills.get_mut(&bill_id) {
            record.state = state;
        }
    }

    fn open_shift_record(&mut self, shift_id: ShiftId, record: ShiftRecord) {
        self.shifts.insert(shift_id, record);
        self.open_shift = Some(shift_id);
    }

    fn shift(&self, shift_id: ShiftId) -> Option<ShiftRecord> {
        self.shifts.get(&shift_id).copied()
    }

    fn record_shift_count(&mut self, shift_id: ShiftId, counted: Money, state: ShiftState) {
        if let Some(record) = self.shifts.get_mut(&shift_id) {
            record.counted = Some(counted);
            record.state = state;
        }
    }

    fn close_shift_record(&mut self, shift_id: ShiftId) {
        if let Some(record) = self.shifts.get_mut(&shift_id) {
            record.state = ShiftState::Closed;
        }
        if self.open_shift == Some(shift_id) {
            self.open_shift = None;
        }
    }

    /// Adds cash taken on a bill to the open shift's rollup, if a shift is open.
    fn collect_cash(&mut self, amount: Money) -> Result<(), DomainError> {
        if let Some(shift_id) = self.open_shift
            && let Some(record) = self.shifts.get_mut(&shift_id)
        {
            record.cash_collected = record.cash_collected.checked_add(amount)?;
        }
        Ok(())
    }
}

/// The edge application: the store, the session, and the loop that ties them to `pos-core`.
#[derive(Debug)]
pub struct Edge<S> {
    store: S,
    identity: StoreIdentity,
    /// The live session (menu, tax table, capabilities, locale) an inbound `OrderIn` reprices from
    /// and every command reads. Behind an `RwLock<Arc<…>>` so the config-pull loop
    /// ([`crate::config_client`], ADR-0033/ADR-0039) can swap in a rebuilt session while the store is
    /// trading: a reader takes a cheap `Arc` snapshot ([`Edge::session`]) that is coherent for the
    /// duration of its command even if a swap lands mid-flight.
    session_cell: RwLock<Arc<EdgeSession>>,
    clock: SystemClock,
    ids: Mutex<EdgeIdGenerator<SystemClock>>,
    fanout: Fanout,
    projection: Mutex<Projection>,
    /// The gapless receipt-number authority a settle allocates from (ADR-0025). Injected rather than
    /// derived from `S`, so the one loop runs over the real SQLite store and over the fakes alike.
    receipts: Arc<dyn ReceiptAuthority>,
}

/// What [`Edge::open_inbound_order`] needs to write the idempotency ledger row in the order's own
/// transaction ([ADR-0064](../../../docs/adr/0064-edge-order-in.md)): the caller's key and the two
/// acceptance facts the order itself does not carry. `order_id`, `business_date` and
/// `awaiting_staff_confirmation` are the order's own and are filled in by `open_inbound_order`; the
/// `queue_number` is reconstructed on a repeat, never stored.
#[derive(Debug, Clone, Copy)]
pub struct IntakeIntent<'a> {
    /// The channel's wire token — the first half of the idempotency key.
    pub sales_channel: &'a str,
    /// The caller's own reference — the second half of the idempotency key.
    pub external_reference: &'a str,
    /// The accepted total (tax-inclusive), the store's own menu total.
    pub total: Money,
    /// Whether any line's caller-quoted price differed from the store's.
    pub repriced: bool,
}

impl<S> Edge<S> {
    /// A snapshot of the synced session — the menu catalog and tax table an inbound `OrderIn`
    /// reprices from. A cheap `Arc` clone, so a caller reads a coherent session for the whole of its
    /// command even if the config-pull loop swaps in a new one mid-flight.
    #[must_use]
    #[expect(
        clippy::missing_panics_doc,
        reason = "the only panic is a poisoned session lock, unreachable here: the critical section \
                  just clones an Arc and cannot itself panic"
    )]
    pub fn session(&self) -> Arc<EdgeSession> {
        self.session_cell
            .read()
            .expect("session lock is not poisoned")
            .clone()
    }

    /// Swaps in a rebuilt session — the config-pull loop's apply step
    /// ([ADR-0039](../../../docs/adr/0039-config-delivery.md)). Commands already in flight keep the
    /// [`Arc`] snapshot they took; the next command sees the new session. Cloud-owned configuration
    /// (ADR-0004): the store never edits this, it only receives it.
    #[expect(
        clippy::missing_panics_doc,
        reason = "the only panic is a poisoned session lock, unreachable here: the critical section \
                  just replaces an Arc and cannot itself panic"
    )]
    pub fn apply_session(&self, session: EdgeSession) {
        *self
            .session_cell
            .write()
            .expect("session lock is not poisoned") = Arc::new(session);
    }

    /// The lines currently marked prepared (`kitchen.ticket.bumped`), for a KDS reading the current
    /// state on (re)connect.
    ///
    /// # Panics
    ///
    /// If the projection lock is poisoned — unreachable, as its critical section only reads the set.
    #[must_use]
    pub fn bumped_line_ids(&self) -> Vec<OrderLineId> {
        self.projection
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .bumped_line_ids()
    }
}

impl<S: EventStore> Edge<S> {
    /// Composes an edge over `store` for a given identity and session, allocating receipt numbers
    /// from `receipts`.
    ///
    /// The real binary passes the SQLite store itself (its writer thread is the gapless authority,
    /// ADR-0025); the example and tests pass an [`InMemoryReceipts`](crate::receipt::InMemoryReceipts).
    ///
    /// # Errors
    ///
    /// [`getrandom::Error`] if the OS entropy source needed to seed the id generator is unavailable.
    pub fn new(
        store: S,
        identity: StoreIdentity,
        session: EdgeSession,
        receipts: Arc<dyn ReceiptAuthority>,
    ) -> Result<Self, getrandom::Error> {
        Ok(Self {
            store,
            identity,
            session_cell: RwLock::new(Arc::new(session)),
            clock: SystemClock,
            ids: Mutex::new(EdgeIdGenerator::new(SystemClock)?),
            fanout: Fanout::new(),
            projection: Mutex::new(Projection::default()),
            receipts,
        })
    }

    /// The fan-out devices subscribe to.
    #[must_use]
    pub fn fanout(&self) -> &Fanout {
        &self.fanout
    }

    /// The store this edge is, so a driving-port caller (the inbound `OrderIn`) can bind an order to
    /// it and refuse one addressed to another store.
    #[must_use]
    pub fn store_id(&self) -> StoreId {
        self.identity.store_id
    }

    /// Records that this box completed device activation and may now trade
    /// (`device.activation.completed`, [ADR-0050](../../../docs/adr/0050-activation-code-exchange.md)).
    ///
    /// A system event, not a domain decision: there is no signed-in employee at first boot, so the
    /// envelope carries none, and the box's own new identity is both the reporting and the activated
    /// device. The caller stores the credential in the [`KeyVault`](pos_ports::KeyVault) *before*
    /// calling this — the vault is what the boot gate reads, so it, not this event, is the source of
    /// truth for "activated"; this event is the notification that flows to the cloud.
    ///
    /// # Errors
    ///
    /// [`AppError`] if the business date cannot be derived, the event cannot be encoded, or the store
    /// cannot be written.
    pub async fn record_activation(&self, activated_device_id: DeviceId) -> Result<(), AppError> {
        let now = self.clock.now();
        let session = self.session();
        let business_date = derive_business_date(now, &session.timezone, session.cutoff)
            .map_err(|_ignored| AppError::Clock)?;
        let payload = DeviceActivationCompleted {
            activated_device_id,
        };
        let data = RawPayload::encode(&payload).map_err(AppError::Encode)?;
        let envelope = EventEnvelope {
            event_id: pos_proto::ids::EventId::new(self.next_ulid()),
            event_type: EventTypeRef::from_known(DeviceActivationCompleted::EVENT_TYPE),
            event_time: now,
            business_date,
            schema_version: DeviceActivationCompleted::SCHEMA_VERSION,
            tenant_id: self.identity.tenant_id,
            brand_id: self.identity.brand_id,
            store_id: self.identity.store_id,
            // At first boot the box's new identity is the device that was activated; there is no
            // separate reporting device and no shift open yet.
            device_id: activated_device_id,
            employee_id: None,
            shift_id: None,
            data,
        };
        let published = serde_json::to_value(&payload).unwrap_or(serde_json::Value::Null);
        let message = ServerMessage::Event {
            event_type: EventTypeRef::from_known(DeviceActivationCompleted::EVENT_TYPE)
                .as_str()
                .to_owned(),
            payload: published,
        };
        self.append_and_publish(vec![envelope], vec![message]).await
    }

    /// Opens an order that arrived from **outside** the store — a marketplace order, the public API,
    /// or a QR guest ([ADR-0064](../../../docs/adr/0064-edge-order-in.md)). Emits `sales.order.opened`
    /// (tableless-capable) and one `sales.order_line.added` per already-priced line, in **one**
    /// transaction, then folds them into the projection; returns the new order's id and the business
    /// date it was stamped with — the caller keys the daily queue number on that same date so the
    /// number belongs to the trading day the order actually opened on.
    ///
    /// Unlike the floor commands there is no signed-in employee, so the events carry the box's own
    /// `device_id` and no employee — the same shape [`Self::record_activation`] uses. The lines are
    /// already priced by the caller (`pos_core::menu::reprice_line`); this records what it decided.
    ///
    /// # Errors
    ///
    /// [`AppError`] if the business date cannot be derived, an event cannot be encoded, or the store
    /// cannot be written.
    pub async fn open_inbound_order(
        &self,
        device_id: DeviceId,
        channel: Open<SalesChannel>,
        table_id: Option<TableId>,
        lines: &[(PricedLine, bool)],
        intake: Option<IntakeIntent<'_>>,
    ) -> Result<(OrderId, BusinessDate), AppError>
    where
        S: IntakeLedger,
    {
        let now = self.clock.now();
        let session = self.session();
        let business_date = derive_business_date(now, &session.timezone, session.cutoff)
            .map_err(|_ignored| AppError::Clock)?;
        let order_id = self.next_order_id();

        let mut envelopes = Vec::with_capacity(lines.len() + 1);
        let mut messages = Vec::with_capacity(lines.len() + 1);

        let opened = SalesOrderOpened {
            order_id,
            channel,
            table_id,
            guest_count: None,
        };
        let (envelope, message) = self.system_prepare(device_id, now, business_date, &opened)?;
        envelopes.push(envelope);
        messages.push(message);

        let mut line_records: Vec<(OrderLineId, LineRecord)> = Vec::with_capacity(lines.len());
        for (priced, note_present) in lines {
            let order_line_id = OrderLineId::new(self.next_ulid());
            let added = SalesOrderLineAdded {
                order_id,
                order_line_id,
                menu_item_id: priced.menu_item_id,
                display_name: priced.display_name.clone(),
                quantity: priced.quantity,
                unit_price: priced.unit_price,
                line_total: priced.line_total,
                tax_class_id: priced.tax_class_id,
                tax_rate: priced.tax_rate,
                seat: None,
                course_id: None,
                note_present: *note_present,
            };
            let (envelope, message) = self.system_prepare(device_id, now, business_date, &added)?;
            envelopes.push(envelope);
            messages.push(message);
            line_records.push((
                order_line_id,
                LineRecord {
                    order_id,
                    state: OrderLineState::Added,
                    menu_item_id: priced.menu_item_id,
                    quantity: priced.quantity,
                    course_id: None,
                    line_total: priced.line_total,
                    tax_class_id: priced.tax_class_id,
                },
            ));
        }

        // The events AND the idempotency ledger row commit in ONE transaction, so a crash between
        // opening the order and recording it is impossible — either both land or neither
        // (ADR-0064). A plain insert on an existing key fails the commit with `already_exists` and
        // rolls the events back with it, which is how a concurrent second order on the same key is
        // refused rather than duplicated.
        let mut tx = self.store.begin().await?;
        self.store.append(&mut tx, &envelopes).await?;
        if let Some(intent) = intake {
            let record = IntakeRecord {
                order_id,
                business_date,
                total: intent.total,
                repriced: intent.repriced,
                awaiting_staff_confirmation: table_id.is_some(),
            };
            self.store
                .record(
                    &mut tx,
                    self.identity.store_id,
                    intent.sales_channel,
                    intent.external_reference,
                    &record,
                )
                .await?;
        }
        tx.commit().await?;
        for message in &messages {
            self.fanout.publish(message);
        }

        {
            let mut projection = self.lock_projection();
            // A QR order names a table, so the floor shows it occupied and staff can open the bill;
            // a delivery or public-API order has none.
            if let Some(table_id) = table_id {
                projection.set_table(table_id, TableState::Occupied);
                projection.open_order(table_id, order_id);
            }
            for (line_id, record) in line_records {
                projection.add_line(line_id, record);
            }
        }
        Ok((order_id, business_date))
    }

    /// The idempotency record a caller's `(sales_channel, external_reference)` already produced at
    /// this store, or `None` ([ADR-0064](../../../docs/adr/0064-edge-order-in.md)). The intake path
    /// reads this to return the same order on a retry rather than opening a second one.
    ///
    /// # Errors
    ///
    /// [`AppError::Port`] if the ledger cannot be read.
    pub async fn look_up_intake(
        &self,
        sales_channel: &str,
        external_reference: &str,
    ) -> Result<Option<IntakeRecord>, AppError>
    where
        S: IntakeLedger,
    {
        self.store
            .look_up(self.identity.store_id, sales_channel, external_reference)
            .await
            .map_err(AppError::Port)
    }

    /// The current projected state of a table.
    #[must_use]
    pub fn table_state(&self, table_id: TableId) -> TableState {
        self.projection
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .table_state(table_id)
    }

    /// Seats guests at a table, opening an order on it (`sales.table.opened`).
    ///
    /// # Errors
    ///
    /// [`AppError`] if the transition is illegal, the store cannot be written, or the business date
    /// cannot be derived.
    pub async fn seat_table(&self, actor: Actor, table_id: TableId) -> Result<TableView, AppError> {
        let ctx = self.decision_ctx(actor)?;
        let current = self.table_state(table_id);
        let decision = decide_table(current, TableCommand::Seat, &ctx)?;

        let order_id = self.next_order_id();
        let payload = SalesTableOpened { table_id, order_id };
        self.commit_and_publish(&ctx, &payload).await?;

        // After the commit the change is durable, so it is safe to show and to remember.
        {
            let mut projection = self.lock_projection();
            projection.set_table(table_id, decision.next_state);
            projection.open_order(table_id, order_id);
        }
        Ok(TableView {
            table_id,
            state: decision.next_state,
        })
    }

    /// Cleans a table down and releases it (`sales.table.closed`).
    ///
    /// # Errors
    ///
    /// [`AppError`] as [`Self::seat_table`].
    pub async fn clean_table(
        &self,
        actor: Actor,
        table_id: TableId,
    ) -> Result<TableView, AppError> {
        let ctx = self.decision_ctx(actor)?;
        let current = self.table_state(table_id);
        let decision = decide_table(current, TableCommand::Clean, &ctx)?;

        let payload = SalesTableClosed { table_id };
        self.commit_and_publish(&ctx, &payload).await?;
        self.lock_projection()
            .set_table(table_id, decision.next_state);
        Ok(TableView {
            table_id,
            state: decision.next_state,
        })
    }

    /// The current state of a line, if the edge knows it.
    #[must_use]
    pub fn line_state(&self, order_line_id: OrderLineId) -> Option<OrderLineState> {
        self.lock_projection().line(order_line_id).map(|r| r.state)
    }

    /// Adds a line to the order the table holds (`sales.order_line.added`).
    ///
    /// Adding a line is not a state-machine transition and needs no permission; it records what the
    /// device captured. A new line starts [`OrderLineState::Added`].
    ///
    /// # Errors
    ///
    /// [`AppError::NoOpenOrder`] if the table has not been seated, or [`AppError`] if the store cannot
    /// be written.
    pub async fn add_line(
        &self,
        actor: Actor,
        table_id: TableId,
        draft: LineDraft,
    ) -> Result<LineView, AppError> {
        let ctx = self.decision_ctx(actor)?;
        let order_id = self
            .lock_projection()
            .order_for_table(table_id)
            .ok_or(AppError::NoOpenOrder)?;
        let order_line_id = OrderLineId::new(self.next_ulid());

        let payload = SalesOrderLineAdded {
            order_id,
            order_line_id,
            menu_item_id: draft.menu_item_id,
            display_name: draft.display_name,
            quantity: draft.quantity,
            unit_price: draft.unit_price,
            line_total: draft.line_total,
            tax_class_id: draft.tax_class_id,
            tax_rate: draft.tax_rate,
            seat: draft.seat,
            course_id: draft.course_id,
            note_present: draft.note_present,
        };
        self.commit_and_publish(&ctx, &payload).await?;

        self.lock_projection().add_line(
            order_line_id,
            LineRecord {
                order_id,
                state: OrderLineState::Added,
                menu_item_id: draft.menu_item_id,
                quantity: draft.quantity,
                course_id: draft.course_id,
                line_total: draft.line_total,
                tax_class_id: draft.tax_class_id,
            },
        );
        Ok(LineView {
            order_id,
            order_line_id,
            state: OrderLineState::Added,
        })
    }

    /// Fires a line to the kitchen (`sales.order_line.fired`), consuming its recipe (§8).
    ///
    /// The consumption `pos-core` computes is a stock-ledger concern applied by the projection (a
    /// later slice); with the bootstrap empty recipe book there is nothing to consume, and the fire
    /// event is the durable record that stock left at fire, not at payment.
    ///
    /// The station the line routes to is derived from the published routing plan (ADR-0072), not
    /// dictated by the caller: `station_id` is an optional fallback used only when the store has no
    /// station plan yet (no rule matched and none is the default). A device therefore fires a line
    /// without needing to know the kitchen's stations, and the plan alone decides where it prints.
    ///
    /// # Errors
    ///
    /// [`AppError::UnknownLine`] for a line the edge does not know, [`AppError::UnroutableLine`] when
    /// neither the plan nor the caller yields a station, [`AppError::Domain`] if firing is not a legal
    /// move (already fired or voided) or the course capability is off, or [`AppError`] if the store
    /// cannot be written.
    pub async fn fire_line(
        &self,
        actor: Actor,
        order_line_id: OrderLineId,
        station_id: Option<StationId>,
    ) -> Result<LineView, AppError> {
        let ctx = self.decision_ctx(actor)?;
        let session = self.session();
        let record = self
            .lock_projection()
            .line(order_line_id)
            .ok_or(AppError::UnknownLine)?;

        // The published routing decides the station; the caller's station is only a fallback for a
        // store that has not published a station plan yet (never-blank, ADR-0072).
        let station_id = session
            .resolve_station(record.menu_item_id, record.course_id)
            .or(station_id)
            .ok_or(AppError::UnroutableLine)?;

        let command = LineCommand::Fire {
            base_item: record.menu_item_id,
            modifiers: Vec::new(),
            quantity: record.quantity,
            course: record.course_id,
        };
        let decision = decide_line(record.state, command, &ctx, &session.recipes)?;

        let payload = SalesOrderLineFired {
            order_id: record.order_id,
            order_line_id,
            station_id,
            fire_time: ctx.now,
        };
        self.commit_and_publish(&ctx, &payload).await?;

        self.lock_projection()
            .set_line_state(order_line_id, decision.next_state);
        Ok(LineView {
            order_id: record.order_id,
            order_line_id,
            state: decision.next_state,
        })
    }

    /// Bumps a kitchen ticket — a station marks lines prepared (`kitchen.ticket.bumped`,
    /// [ADR-0026](../../../docs/adr/0026-port-shapes.md) event catalogue §18). Durable and fanned out,
    /// so a second KDS agrees the ticket is done rather than each screen holding a private, divergent
    /// "done" flag. A bump is orthogonal to a line's order state (a made line is still "fired"), so it
    /// is recorded on the projection's bumped set, not as a state-machine transition.
    ///
    /// # Errors
    ///
    /// [`AppError`] if the business date cannot be derived, the event cannot be encoded, or the store
    /// cannot be written.
    pub async fn bump_ticket(
        &self,
        actor: Actor,
        order_id: OrderId,
        station_id: StationId,
        order_line_ids: Vec<OrderLineId>,
    ) -> Result<BumpView, AppError> {
        let ctx = self.decision_ctx(actor)?;
        let payload = KitchenTicketBumped {
            order_id,
            station_id,
            order_line_ids: order_line_ids.clone(),
        };
        self.commit_and_publish(&ctx, &payload).await?;
        self.lock_projection().mark_bumped(&order_line_ids);
        Ok(BumpView {
            order_id,
            station_id,
            order_line_ids,
        })
    }

    /// Opens a bill on the order a table holds (`billing.bill.opened`) and moves the table to
    /// awaiting payment.
    ///
    /// Splitting, merging and settling all presuppose a bill that something created; opening it is
    /// the "request bill" moment of the floor cycle, so the table moves `Occupied → AwaitingPayment`
    /// here. The table's fine-grained state is derived from the bill lifecycle rather than from its
    /// own events, because the frozen catalogue has no table-transition event for it.
    ///
    /// # Errors
    ///
    /// [`AppError::Domain`] if the table is not occupied (requesting the bill is illegal otherwise),
    /// [`AppError::NoOpenOrder`] if the table has no order, or [`AppError`] if the store cannot be
    /// written.
    pub async fn open_bill(&self, actor: Actor, table_id: TableId) -> Result<BillView, AppError> {
        let ctx = self.decision_ctx(actor)?;
        let current_table = self.table_state(table_id);
        // Requesting the bill is a legal floor move only from Occupied; decide_table also gates the
        // tables capability.
        let table_decision = decide_table(current_table, TableCommand::RequestBill, &ctx)?;

        let order_id = self
            .lock_projection()
            .order_for_table(table_id)
            .ok_or(AppError::NoOpenOrder)?;
        let bill_id = BillId::new(self.next_ulid());

        let payload = BillingBillOpened { bill_id, order_id };
        self.commit_and_publish(&ctx, &payload).await?;

        {
            let mut projection = self.lock_projection();
            projection.open_bill_record(
                bill_id,
                BillRecord {
                    order_id,
                    table_id,
                    state: BillState::Open,
                },
            );
            projection.set_table(table_id, table_decision.next_state);
        }
        Ok(BillView {
            bill_id,
            state: BillState::Open,
            receipt_number: None,
            total_due: None,
            table_state: table_decision.next_state,
            print_receipt: false,
        })
    }

    /// Settles a bill (`billing.bill.settled`) and cycles its table to needs-cleaning.
    ///
    /// Assembles what is owed from the order's captured line totals ([`billing::assemble`], §14.2),
    /// proves the payments sum **exactly** to it ([`billing::settle`] via [`decide_bill`], ADR-0028),
    /// then allocates the gapless per-store receipt number for this bill (ADR-0025) before appending
    /// the event that carries it — so a crash after allocating reuses the number rather than skipping
    /// one. The receipt is **not** a legal invoice number.
    ///
    /// The [`Effect::PrintReceipt`] the domain returns is surfaced on [`BillView::print_receipt`] for
    /// the caller to run after commit; the edge holds no printer, so a rolled-back settle prints
    /// nothing.
    ///
    /// # Errors
    ///
    /// [`AppError::UnknownBill`] for a bill the edge does not know; [`AppError::Domain`] if the bill
    /// is not open, the table is not awaiting payment, the payments do not sum to the total, or a
    /// line's tax class has no configured rate; or [`AppError`] if the store cannot be written.
    pub async fn settle_bill(
        &self,
        actor: Actor,
        bill_id: BillId,
        payments: Vec<Payment>,
        tips: Vec<Money>,
    ) -> Result<BillView, AppError> {
        let ctx = self.decision_ctx(actor)?;
        let bill = self
            .lock_projection()
            .bill(bill_id)
            .ok_or(AppError::UnknownBill)?;

        // Assemble the amount owed from the order's captured line totals, grouped per tax class.
        let lines = self.lock_projection().lines_for_order(bill.order_id);
        let class_bases = Self::class_bases(&lines)?;
        let session = self.session();
        let totals = billing::assemble(&Self::bill_input(&session, &class_bases))?;

        // The table cycles AwaitingPayment -> NeedsCleaning; prove that move is legal.
        let current_table = self.table_state(bill.table_id);
        let table_decision = decide_table(current_table, TableCommand::Settle, &ctx)?;

        // The cash tenders (not card, not tips, not rounding) are what lands in the drawer for the
        // open shift's blind-close roll-up. Summed before the payments move into the command.
        let mut cash_taken = Money::zero(self.session().currency);
        for payment in &payments {
            if payment.method == PaymentMethod::Cash {
                cash_taken = cash_taken
                    .checked_add(payment.applied_to_bill)
                    .map_err(DomainError::from)?;
            }
        }

        // Prove the settlement invariant against the assembled total, and that the bill can settle.
        // Done before allocating a number, so a refused settle consumes none. The payments are cloned
        // because each is recorded again below as a captured-payment event.
        let bill_decision = decide_bill(
            bill.state,
            BillCommand::Settle {
                total_due: totals.total_due,
                payments: payments.clone(),
                tips,
            },
            &ctx,
        )?;

        // Allocate the gapless receipt number for this bill, then append the events that record it.
        let receipt_number = self
            .receipts
            .allocate_receipt(self.identity.store_id, bill_id)
            .await?;

        let reduction_total = totals
            .discount_total
            .checked_add(totals.comp_total)
            .map_err(DomainError::from)?;

        // One `billing.payment.captured` per tender, then `billing.bill.settled`, all in one
        // transaction so a crash never leaves a receipt without its payments (or the reverse). The
        // captured payments are what let the shift cash roll-up be rebuilt from the log; each records
        // its own change, and tips are held apart from the sale (per-payment tip capture is P7).
        let zero = Money::zero(self.session().currency);
        let mut envelopes = Vec::with_capacity(payments.len() + 1);
        let mut messages = Vec::with_capacity(payments.len() + 1);
        for payment in &payments {
            let change = payment
                .tendered
                .checked_sub(payment.applied_to_bill)
                .map_err(DomainError::from)?;
            let captured = BillingPaymentCaptured {
                bill_id,
                payment_id: PaymentId::new(self.next_ulid()),
                method: Open::from_known(payment.method),
                outcome: Open::from_known(PaymentOutcome::Captured),
                tendered: payment.tendered,
                applied_to_bill: payment.applied_to_bill,
                change_given: if change.is_negative() { zero } else { change },
                tip_amount: zero,
            };
            let (envelope, message) = self.prepare(&ctx, &captured)?;
            envelopes.push(envelope);
            messages.push(message);
        }

        let settled = BillingBillSettled {
            bill_id,
            receipt_number,
            subtotal: totals.subtotal,
            reduction_total,
            service_charge: totals.service_charge,
            tax_total: totals.tax_total,
            rounding_adjustment: totals.rounding_adjustment,
            total_due: totals.total_due,
        };
        let (settled_envelope, settled_message) = self.prepare(&ctx, &settled)?;
        envelopes.push(settled_envelope);
        messages.push(settled_message);

        self.append_and_publish(envelopes, messages).await?;

        {
            let mut projection = self.lock_projection();
            projection.set_bill_state(bill_id, bill_decision.next_state);
            projection.set_table(bill.table_id, table_decision.next_state);
            projection.collect_cash(cash_taken)?;
        }
        Ok(BillView {
            bill_id,
            state: bill_decision.next_state,
            receipt_number: Some(receipt_number),
            total_due: Some(totals.total_due),
            table_state: table_decision.next_state,
            print_receipt: bill_decision.effects.contains(&Effect::PrintReceipt),
        })
    }

    /// Opens a cash shift with a starting float (`cash.shift.opened`).
    ///
    /// One shift is open per device (§6, archive): opening a second while one is still open is
    /// refused. Subsequent events carry this shift's id until it closes.
    ///
    /// # Errors
    ///
    /// [`AppError::ShiftAlreadyOpen`] if a shift is already open, or [`AppError`] if the store cannot
    /// be written.
    pub async fn open_shift(
        &self,
        actor: Actor,
        opening_float: Money,
    ) -> Result<ShiftView, AppError> {
        if self.current_shift_id().is_some() {
            return Err(AppError::ShiftAlreadyOpen);
        }
        let ctx = self.decision_ctx(actor)?;
        let shift_id = ShiftId::new(self.next_ulid());

        let payload = CashShiftOpened {
            opened_shift_id: shift_id,
            opening_float,
        };
        self.commit_and_publish(&ctx, &payload).await?;

        self.lock_projection().open_shift_record(
            shift_id,
            ShiftRecord {
                state: ShiftState::Open,
                opening_float,
                cash_collected: Money::zero(self.session().currency),
                counted: None,
            },
        );
        Ok(ShiftView {
            shift_id,
            state: ShiftState::Open,
            expected_amount: None,
            counted_amount: None,
            variance: None,
            print_shift_report: false,
        })
    }

    /// Records the blind count on a shift (`cash.shift.counted`).
    ///
    /// The counted cash is entered **before** the system reveals what it expected, so this returns
    /// no expectation and no variance — that is what makes the close blind (§11.1). `counted_minor`
    /// is the physical count in minor units.
    ///
    /// # Errors
    ///
    /// [`AppError::UnknownShift`] for a shift the edge does not know, [`AppError::Domain`] if the
    /// shift is not open, or [`AppError`] if the store cannot be written.
    pub async fn count_shift(
        &self,
        actor: Actor,
        shift_id: ShiftId,
        counted_minor: i64,
    ) -> Result<ShiftView, AppError> {
        let ctx = self.decision_ctx(actor)?;
        let record = self
            .lock_projection()
            .shift(shift_id)
            .ok_or(AppError::UnknownShift)?;
        let decision = decide_shift(record.state, ShiftCommand::Count { counted_minor }, &ctx)?;

        let counted_amount = Money::new(self.session().currency, counted_minor);
        let payload = CashShiftCounted {
            counted_shift_id: shift_id,
            counted_amount,
            count_time: ctx.now,
        };
        self.commit_and_publish(&ctx, &payload).await?;

        self.lock_projection()
            .record_shift_count(shift_id, counted_amount, decision.next_state);
        Ok(ShiftView {
            shift_id,
            state: decision.next_state,
            expected_amount: None,
            counted_amount: Some(counted_amount),
            variance: None,
            print_shift_report: false,
        })
    }

    /// Closes a counted shift (`cash.shift.closed`), revealing the expected amount and the variance.
    ///
    /// The expectation is the shift's cash roll-up — opening float plus the cash its bills took — and
    /// is revealed only now, after the blind count. The variance is `counted − expected`; negative is
    /// short. Closing surfaces [`Effect::PrintShiftReport`] on the view for the caller to run.
    ///
    /// # Errors
    ///
    /// [`AppError::UnknownShift`] for a shift the edge does not know, [`AppError::Domain`] if the
    /// shift has not been counted (a blind close cannot skip the count) or closing is not granted, or
    /// [`AppError`] if the store cannot be written.
    pub async fn close_shift(
        &self,
        actor: Actor,
        shift_id: ShiftId,
    ) -> Result<ShiftView, AppError> {
        let ctx = self.decision_ctx(actor)?;
        let record = self
            .lock_projection()
            .shift(shift_id)
            .ok_or(AppError::UnknownShift)?;
        let decision = decide_shift(record.state, ShiftCommand::Close, &ctx)?;

        // Reaching Close means the shift was counted, so the count is present; the fallback never
        // fires but keeps this off any panic path.
        let counted_amount = record
            .counted
            .unwrap_or_else(|| Money::zero(self.session().currency));
        let expected_amount = record.expected()?;
        let variance = counted_amount
            .checked_sub(expected_amount)
            .map_err(DomainError::from)?;

        let payload = CashShiftClosed {
            closed_shift_id: shift_id,
            expected_amount,
            counted_amount,
            variance,
        };
        self.commit_and_publish(&ctx, &payload).await?;

        self.lock_projection().close_shift_record(shift_id);
        Ok(ShiftView {
            shift_id,
            state: ShiftState::Closed,
            expected_amount: Some(expected_amount),
            counted_amount: Some(counted_amount),
            variance: Some(variance),
            print_shift_report: decision.effects.contains(&Effect::PrintShiftReport),
        })
    }

    /// Rebuilds the in-memory projection from the durable event log (P5 crash recovery).
    ///
    /// The log in the store is the truth; the projection is a cache. On boot the edge replays every
    /// event in `event_id` order and folds it back, so a committed sale — a seated table, a fired
    /// line, a settled bill with its receipt, an open shift and the cash it has taken — survives a
    /// restart, and only an *uncommitted* transaction is lost. Idempotent: replaying committed facts
    /// lands on the same state each time.
    ///
    /// # Errors
    ///
    /// [`AppError::Port`] if the log cannot be read, or [`AppError::Encode`] if a stored event cannot
    /// be decoded (a corrupt log).
    pub async fn rebuild(&self) -> Result<(), AppError> {
        // A page large enough that a normal store's day is a handful of reads, small enough that the
        // batch and its decode stay bounded.
        let limit = NonZeroU32::new(512).unwrap_or(NonZeroU32::MIN);
        let mut after: Option<pos_proto::ids::EventId> = None;
        loop {
            let mut query = EventQuery::first(self.identity.store_id, limit);
            if let Some(cursor) = after {
                query = query.after(cursor);
            }
            let batch = self.store.read(&query).await?;
            let Some(last) = batch.last() else { break };
            after = Some(last.event_id);
            let mut projection = self.lock_projection();
            for envelope in &batch {
                Self::fold(&mut projection, envelope)?;
            }
        }
        Ok(())
    }

    /// Folds one logged event back into the projection during [`Self::rebuild`].
    ///
    /// The reverse of what each command emits, without re-deciding: the events already happened, so
    /// this trusts them. Events the projection does not track — and unknown types from a newer
    /// writer — are skipped, which is what keeps a forward-compatible log replayable.
    fn fold(
        projection: &mut Projection,
        envelope: &EventEnvelope<RawPayload>,
    ) -> Result<(), AppError> {
        let Some(known) = envelope.event_type.known() else {
            return Ok(());
        };
        match known {
            EventType::SalesTableOpened => {
                let event: SalesTableOpened = envelope.data.decode().map_err(AppError::Encode)?;
                projection.set_table(event.table_id, TableState::Occupied);
                projection.open_order(event.table_id, event.order_id);
            }
            EventType::SalesTableClosed => {
                let event: SalesTableClosed = envelope.data.decode().map_err(AppError::Encode)?;
                projection.set_table(event.table_id, TableState::Free);
            }
            EventType::SalesOrderLineAdded => {
                let event: SalesOrderLineAdded =
                    envelope.data.decode().map_err(AppError::Encode)?;
                projection.add_line(
                    event.order_line_id,
                    LineRecord {
                        order_id: event.order_id,
                        state: OrderLineState::Added,
                        menu_item_id: event.menu_item_id,
                        quantity: event.quantity,
                        course_id: event.course_id,
                        line_total: event.line_total,
                        tax_class_id: event.tax_class_id,
                    },
                );
            }
            EventType::SalesOrderLineFired => {
                let event: SalesOrderLineFired =
                    envelope.data.decode().map_err(AppError::Encode)?;
                projection.set_line_state(event.order_line_id, OrderLineState::Fired);
            }
            EventType::BillingBillOpened => {
                let event: BillingBillOpened = envelope.data.decode().map_err(AppError::Encode)?;
                if let Some(table_id) = projection.table_for_order(event.order_id) {
                    projection.open_bill_record(
                        event.bill_id,
                        BillRecord {
                            order_id: event.order_id,
                            table_id,
                            state: BillState::Open,
                        },
                    );
                    projection.set_table(table_id, TableState::AwaitingPayment);
                }
            }
            EventType::BillingPaymentCaptured => {
                let event: BillingPaymentCaptured =
                    envelope.data.decode().map_err(AppError::Encode)?;
                // Only cash reaches the drawer, and the roll-up is the reason payments are evented.
                if event.method.known() == PaymentMethod::Cash {
                    projection.collect_cash(event.applied_to_bill)?;
                }
            }
            EventType::BillingBillSettled => {
                let event: BillingBillSettled = envelope.data.decode().map_err(AppError::Encode)?;
                projection.set_bill_state(event.bill_id, BillState::Settled);
                if let Some(record) = projection.bill(event.bill_id) {
                    projection.set_table(record.table_id, TableState::NeedsCleaning);
                }
            }
            EventType::CashShiftOpened => {
                let event: CashShiftOpened = envelope.data.decode().map_err(AppError::Encode)?;
                projection.open_shift_record(
                    event.opened_shift_id,
                    ShiftRecord {
                        state: ShiftState::Open,
                        opening_float: event.opening_float,
                        cash_collected: Money::zero(event.opening_float.currency_code),
                        counted: None,
                    },
                );
            }
            EventType::CashShiftCounted => {
                let event: CashShiftCounted = envelope.data.decode().map_err(AppError::Encode)?;
                projection.record_shift_count(
                    event.counted_shift_id,
                    event.counted_amount,
                    ShiftState::Counted,
                );
            }
            EventType::CashShiftClosed => {
                let event: CashShiftClosed = envelope.data.decode().map_err(AppError::Encode)?;
                projection.close_shift_record(event.closed_shift_id);
            }
            EventType::KitchenTicketBumped => {
                let event: KitchenTicketBumped =
                    envelope.data.decode().map_err(AppError::Encode)?;
                projection.mark_bumped(&event.order_line_ids);
            }
            _ => {}
        }
        Ok(())
    }

    /// Groups an order's lines into a pre-tax base per tax class, the input [`billing::assemble`]
    /// takes. A voided line is owed nothing, so it contributes nothing.
    fn class_bases(lines: &[LineRecord]) -> Result<Vec<ClassBase>, AppError> {
        let mut bases: Vec<ClassBase> = Vec::new();
        for line in lines {
            if line.state == OrderLineState::Voided {
                continue;
            }
            if let Some(base) = bases
                .iter_mut()
                .find(|base| base.tax_class_id == line.tax_class_id)
            {
                base.amount = base
                    .amount
                    .checked_add(line.line_total)
                    .map_err(DomainError::from)?;
            } else {
                bases.push(ClassBase {
                    tax_class_id: line.tax_class_id,
                    amount: line.line_total,
                });
            }
        }
        Ok(bases)
    }

    /// Builds the bill-assembly input from the session's tax configuration. The P5 bootstrap runs
    /// with no bill-level discount, no service charge and no cash rounding; the cloud config tree
    /// (P7) supplies those, and the shape here is ready for them.
    fn bill_input<'a>(session: &'a EdgeSession, class_bases: &'a [ClassBase]) -> BillInput<'a> {
        let currency = session.currency;
        BillInput {
            currency_code: currency,
            class_bases,
            bill_discount: Money::zero(currency),
            comps: Money::zero(currency),
            service_charge: Money::zero(currency),
            service_charge_taxable: true,
            service_charge_tax_class: None,
            rates: &session.tax_rates,
            sales_channel: session.sales_channel,
            cash_rounding_increment: None,
            rounding_mode: Rounding::HalfUp,
        }
    }

    /// The commit-and-publish half of a single-event command: write the event in one transaction,
    /// then — and only then — tell every device. In that order, so a rolled-back write is never seen.
    /// Updating the projection is the caller's, done after this returns for the same reason.
    async fn commit_and_publish<P: EventPayload + serde::Serialize>(
        &self,
        ctx: &DecisionCtx,
        payload: &P,
    ) -> Result<(), AppError> {
        let (envelope, message) = self.prepare(ctx, payload)?;
        self.append_and_publish(vec![envelope], vec![message]).await
    }

    /// Prepares an event for the store and the fan-out: the stamped envelope to append, and the
    /// message to broadcast once it commits.
    fn prepare<P: EventPayload + serde::Serialize>(
        &self,
        ctx: &DecisionCtx,
        payload: &P,
    ) -> Result<(EventEnvelope<RawPayload>, ServerMessage), AppError> {
        let published = serde_json::to_value(payload).unwrap_or(serde_json::Value::Null);
        let event_type = EventTypeRef::from_known(P::EVENT_TYPE).as_str().to_owned();
        let envelope = self.envelope(ctx, payload)?;
        Ok((
            envelope,
            ServerMessage::Event {
                event_type,
                payload: published,
            },
        ))
    }

    /// Prepares a **system** event — one with no signed-in employee — building its envelope with the
    /// box's own `device_id` and `employee_id: None`, and its fan-out message. Inbound-order intake
    /// ([`Self::open_inbound_order`]) and activation use this instead of [`Self::prepare`], which
    /// needs a [`DecisionCtx`] actor there is none of.
    fn system_prepare<P: EventPayload + serde::Serialize>(
        &self,
        device_id: DeviceId,
        now: Timestamp,
        business_date: BusinessDate,
        payload: &P,
    ) -> Result<(EventEnvelope<RawPayload>, ServerMessage), AppError> {
        let published = serde_json::to_value(payload).unwrap_or(serde_json::Value::Null);
        let event_type = EventTypeRef::from_known(P::EVENT_TYPE).as_str().to_owned();
        let data = RawPayload::encode(payload).map_err(AppError::Encode)?;
        let envelope = EventEnvelope {
            event_id: pos_proto::ids::EventId::new(self.next_ulid()),
            event_type: EventTypeRef::from_known(P::EVENT_TYPE),
            event_time: now,
            business_date,
            schema_version: P::SCHEMA_VERSION,
            tenant_id: self.identity.tenant_id,
            brand_id: self.identity.brand_id,
            store_id: self.identity.store_id,
            device_id,
            employee_id: None,
            shift_id: self.current_shift_id(),
            data,
        };
        Ok((
            envelope,
            ServerMessage::Event {
                event_type,
                payload: published,
            },
        ))
    }

    /// Appends every envelope in **one** transaction, then — and only then — publishes every message.
    ///
    /// The atomic-append is what lets a settle write its `billing.payment.captured` events and its
    /// `billing.bill.settled` event together: a crash leaves either all of them or none, never a
    /// receipt without its payments. Nothing is published until the commit succeeds.
    async fn append_and_publish(
        &self,
        envelopes: Vec<EventEnvelope<RawPayload>>,
        messages: Vec<ServerMessage>,
    ) -> Result<(), AppError> {
        let mut tx = self.store.begin().await?;
        self.store.append(&mut tx, &envelopes).await?;
        tx.commit().await?;

        for message in &messages {
            self.fanout.publish(message);
        }
        Ok(())
    }

    /// Locks the projection, recovering from a poisoned lock rather than propagating the panic.
    fn lock_projection(&self) -> std::sync::MutexGuard<'_, Projection> {
        self.projection
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Builds the wire envelope for a typed payload, stamping the full context.
    fn envelope<P: EventPayload>(
        &self,
        ctx: &DecisionCtx,
        payload: &P,
    ) -> Result<EventEnvelope<RawPayload>, AppError> {
        let data = RawPayload::encode(payload).map_err(AppError::Encode)?;
        Ok(EventEnvelope {
            event_id: pos_proto::ids::EventId::new(self.next_ulid()),
            event_type: EventTypeRef::from_known(P::EVENT_TYPE),
            event_time: ctx.now,
            business_date: ctx.business_date,
            schema_version: P::SCHEMA_VERSION,
            tenant_id: self.identity.tenant_id,
            brand_id: self.identity.brand_id,
            store_id: self.identity.store_id,
            device_id: ctx.actor.device_id,
            employee_id: Some(ctx.actor.employee_id),
            shift_id: self.current_shift_id(),
            data,
        })
    }

    /// The shift currently trading, so every event minted during it carries its id. `None` when no
    /// shift is open, which is the ordinary state before the first open of the day.
    fn current_shift_id(&self) -> Option<ShiftId> {
        self.lock_projection().open_shift
    }

    /// Assembles the decision context from the clock and the session.
    fn decision_ctx(&self, actor: Actor) -> Result<DecisionCtx, AppError> {
        let now = self.clock.now();
        let session = self.session();
        let business_date = derive_business_date(now, &session.timezone, session.cutoff)
            .map_err(|_| AppError::Clock)?;
        Ok(DecisionCtx {
            now,
            business_date,
            actor,
            granted: session.granted,
            capabilities: session.capabilities,
            connectivity: session.connectivity,
            currency: session.currency,
        })
    }

    fn next_ulid(&self) -> Ulid {
        self.ids
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .next_id()
    }

    fn next_order_id(&self) -> OrderId {
        OrderId::new(self.next_ulid())
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::{Edge, EdgeSession, LineDraft, StoreIdentity};
    use crate::receipt::InMemoryReceipts;
    use pos_core::billing::Payment;
    use pos_core::decision::Actor;
    use pos_fakes::FakeStore;
    use pos_proto::floor::{KitchenStation, RoutingRule, StationPlan};
    use pos_proto::ids::{DeviceId, EmployeeId, MenuItemId, StationId, StoreId, TableId};
    use pos_proto::money::{CurrencyCode, Money, Ratio};
    use pos_proto::quantity::Quantity;
    use pos_proto::text::DisplayName;
    use pos_proto::ulid::Ulid;
    use pos_proto::{BillState, OrderLineState, PaymentMethod, ShiftState, TableState};

    fn identity() -> StoreIdentity {
        StoreIdentity::for_store(StoreId::new(Ulid::from_u128(3)))
    }

    fn actor() -> Actor {
        Actor {
            employee_id: EmployeeId::new(Ulid::from_u128(10)),
            device_id: DeviceId::new(Ulid::from_u128(20)),
        }
    }

    fn edge() -> Edge<FakeStore> {
        Edge::new(
            FakeStore::default(),
            identity(),
            EdgeSession::bootstrap(),
            Arc::new(InMemoryReceipts::new()),
        )
        .expect("seeds")
    }

    fn vnd(minor: i64) -> Money {
        Money::new(CurrencyCode::VND, minor)
    }

    fn a_line() -> LineDraft {
        LineDraft {
            menu_item_id: MenuItemId::new(Ulid::from_u128(500)),
            display_name: DisplayName::new("Margherita"),
            quantity: Quantity::ONE,
            unit_price: vnd(150_000),
            line_total: vnd(150_000),
            tax_class_id: EdgeSession::standard_tax_class(),
            tax_rate: Ratio::basis_points(1_000).expect("a valid rate"),
            seat: None,
            course_id: None,
            note_present: false,
        }
    }

    #[test]
    fn seating_a_table_opens_it() {
        pos_fakes::executor::run_ready(async {
            let edge = edge();
            let table = TableId::new(Ulid::from_u128(100));

            assert_eq!(edge.table_state(table), TableState::Free);
            let view = edge.seat_table(actor(), table).await.expect("seats");
            assert_eq!(view.state, TableState::Occupied);
            assert_eq!(edge.table_state(table), TableState::Occupied);
        });
    }

    #[test]
    fn a_second_device_sees_the_change_over_the_fanout() {
        pos_fakes::executor::run_ready(async {
            let edge = edge();
            let table = TableId::new(Ulid::from_u128(101));
            let mut device_a = edge.fanout().subscribe();
            let mut device_b = edge.fanout().subscribe();

            edge.seat_table(actor(), table).await.expect("seats");

            for device in [&mut device_a, &mut device_b] {
                let frame = device.try_recv().expect("a frame reached the device");
                assert!(frame.contains("sales.table.opened"));
            }
        });
    }

    #[test]
    fn an_illegal_transition_is_refused_and_nothing_is_published() {
        pos_fakes::executor::run_ready(async {
            let edge = edge();
            let table = TableId::new(Ulid::from_u128(102));
            let mut device = edge.fanout().subscribe();

            // Cleaning a Free table is not a legal move (clean is needs-cleaning -> free).
            let refused = edge.clean_table(actor(), table).await;
            assert!(refused.is_err(), "an illegal transition is refused");
            assert!(
                device.try_recv().is_err(),
                "a refused command publishes nothing"
            );
            assert_eq!(
                edge.table_state(table),
                TableState::Free,
                "state is unchanged"
            );
        });
    }

    #[test]
    fn a_line_is_added_to_a_seated_table_then_fired() {
        pos_fakes::executor::run_ready(async {
            let edge = edge();
            let table = TableId::new(Ulid::from_u128(200));
            edge.seat_table(actor(), table).await.expect("seats");

            let line = edge.add_line(actor(), table, a_line()).await.expect("adds");
            assert_eq!(line.state, OrderLineState::Added);
            assert_eq!(
                edge.line_state(line.order_line_id),
                Some(OrderLineState::Added)
            );

            let station = StationId::new(Ulid::from_u128(9));
            let fired = edge
                .fire_line(actor(), line.order_line_id, Some(station))
                .await
                .expect("fires");
            assert_eq!(fired.state, OrderLineState::Fired);
            assert_eq!(
                edge.line_state(line.order_line_id),
                Some(OrderLineState::Fired)
            );
        });
    }

    #[test]
    fn a_line_on_an_unseated_table_is_refused() {
        pos_fakes::executor::run_ready(async {
            let edge = edge();
            let table = TableId::new(Ulid::from_u128(201));
            // Never seated: there is no open order to add to.
            let refused = edge.add_line(actor(), table, a_line()).await;
            assert!(matches!(refused, Err(super::AppError::NoOpenOrder)));
        });
    }

    #[test]
    fn firing_an_already_fired_line_is_refused() {
        pos_fakes::executor::run_ready(async {
            let edge = edge();
            let table = TableId::new(Ulid::from_u128(202));
            edge.seat_table(actor(), table).await.expect("seats");
            let line = edge.add_line(actor(), table, a_line()).await.expect("adds");
            let station = StationId::new(Ulid::from_u128(9));
            edge.fire_line(actor(), line.order_line_id, Some(station))
                .await
                .expect("first fire");

            // A fired line cannot fire again — the domain refuses the transition.
            let refused = edge
                .fire_line(actor(), line.order_line_id, Some(station))
                .await;
            assert!(matches!(refused, Err(super::AppError::Domain(_))));
        });
    }

    #[test]
    fn a_fired_line_routes_to_the_planned_station_over_the_caller() {
        pos_fakes::executor::run_ready(async {
            let edge = edge();
            // Publish a plan that routes the line's item (500) to station 700; the caller will name a
            // different station (999), which the published routing must override (ADR-0072).
            let planned = StationId::new(Ulid::from_u128(700));
            let plan = StationPlan::new()
                .with_station(KitchenStation {
                    station_id: planned,
                    name: DisplayName::new("Oven"),
                    backup_station_id: None,
                })
                .with_rule(RoutingRule {
                    station_id: planned,
                    menu_item_id: Some(MenuItemId::new(Ulid::from_u128(500))),
                    course_id: None,
                });
            edge.apply_session(EdgeSession::bootstrap().with_stations(plan));

            let table = TableId::new(Ulid::from_u128(210));
            edge.seat_table(actor(), table).await.expect("seats");
            let line = edge.add_line(actor(), table, a_line()).await.expect("adds");
            let mut device = edge.fanout().subscribe();

            let caller = StationId::new(Ulid::from_u128(999));
            edge.fire_line(actor(), line.order_line_id, Some(caller))
                .await
                .expect("fires");

            let frame = device.try_recv().expect("a fire frame reached the device");
            assert!(frame.contains("sales.order_line.fired"));
            assert!(
                frame.contains(&planned.to_string()),
                "the line routes to the plan's station, not the caller's"
            );
            assert!(
                !frame.contains(&caller.to_string()),
                "the caller's station is ignored when the plan resolves one"
            );
        });
    }

    #[test]
    fn firing_with_no_station_plan_or_caller_is_unroutable() {
        pos_fakes::executor::run_ready(async {
            // The bootstrap session has an empty station plan; with no caller station either, a fire
            // has nowhere to route and is refused rather than published with a blank station.
            let edge = edge();
            let table = TableId::new(Ulid::from_u128(211));
            edge.seat_table(actor(), table).await.expect("seats");
            let line = edge.add_line(actor(), table, a_line()).await.expect("adds");

            let refused = edge.fire_line(actor(), line.order_line_id, None).await;
            assert!(matches!(refused, Err(super::AppError::UnroutableLine)));
        });
    }

    fn cash(minor: i64) -> Payment {
        Payment {
            method: PaymentMethod::Cash,
            tendered: vnd(minor),
            applied_to_bill: vnd(minor),
        }
    }

    #[test]
    fn a_bill_opens_and_settles_with_a_gapless_receipt() {
        pos_fakes::executor::run_ready(async {
            let edge = edge();
            let table = TableId::new(Ulid::from_u128(300));
            edge.seat_table(actor(), table).await.expect("seats");
            edge.add_line(actor(), table, a_line()).await.expect("adds");

            // Opening the bill requests it: the table moves to awaiting payment.
            let opened = edge.open_bill(actor(), table).await.expect("opens a bill");
            assert_eq!(opened.state, BillState::Open);
            assert_eq!(opened.table_state, TableState::AwaitingPayment);
            assert_eq!(edge.table_state(table), TableState::AwaitingPayment);

            // One 150k line at the 10% standard rate is 165k owed.
            let settled = edge
                .settle_bill(actor(), opened.bill_id, vec![cash(165_000)], vec![])
                .await
                .expect("settles");
            assert_eq!(settled.state, BillState::Settled);
            assert_eq!(settled.total_due, Some(vnd(165_000)));
            assert_eq!(
                settled.receipt_number,
                Some(1),
                "the first receipt is number one"
            );
            assert!(
                settled.print_receipt,
                "settling asks for a receipt to print"
            );
            // The table cycles to needs-cleaning, then a clean releases it.
            assert_eq!(settled.table_state, TableState::NeedsCleaning);
            let cleaned = edge.clean_table(actor(), table).await.expect("cleans");
            assert_eq!(cleaned.state, TableState::Free);
        });
    }

    #[test]
    fn a_bill_settles_split_across_cash_and_card() {
        pos_fakes::executor::run_ready(async {
            let edge = edge();
            let table = TableId::new(Ulid::from_u128(301));
            edge.seat_table(actor(), table).await.expect("seats");
            edge.add_line(actor(), table, a_line()).await.expect("adds");
            let opened = edge.open_bill(actor(), table).await.expect("opens a bill");

            // 65k cash + 100k card == 165k owed: a split tender still sums exactly to the total.
            let card = Payment {
                method: PaymentMethod::Card,
                tendered: vnd(100_000),
                applied_to_bill: vnd(100_000),
            };
            let settled = edge
                .settle_bill(actor(), opened.bill_id, vec![cash(65_000), card], vec![])
                .await
                .expect("settles across two tenders");
            assert_eq!(settled.state, BillState::Settled);
            assert_eq!(settled.receipt_number, Some(1));
        });
    }

    #[test]
    fn settling_twice_is_refused() {
        pos_fakes::executor::run_ready(async {
            let edge = edge();
            let table = TableId::new(Ulid::from_u128(302));
            edge.seat_table(actor(), table).await.expect("seats");
            edge.add_line(actor(), table, a_line()).await.expect("adds");
            let opened = edge.open_bill(actor(), table).await.expect("opens a bill");
            edge.settle_bill(actor(), opened.bill_id, vec![cash(165_000)], vec![])
                .await
                .expect("first settle");

            // A settled bill is terminal: a second settle is refused (a refund is a new movement).
            let refused = edge
                .settle_bill(actor(), opened.bill_id, vec![cash(165_000)], vec![])
                .await;
            assert!(matches!(refused, Err(super::AppError::Domain(_))));
        });
    }

    #[test]
    fn underpaying_a_bill_is_refused() {
        pos_fakes::executor::run_ready(async {
            let edge = edge();
            let table = TableId::new(Ulid::from_u128(303));
            edge.seat_table(actor(), table).await.expect("seats");
            edge.add_line(actor(), table, a_line()).await.expect("adds");
            let opened = edge.open_bill(actor(), table).await.expect("opens a bill");

            // 150k applied against 165k owed does not sum to the total — the invariant refuses it.
            let refused = edge
                .settle_bill(actor(), opened.bill_id, vec![cash(150_000)], vec![])
                .await;
            assert!(matches!(refused, Err(super::AppError::Domain(_))));
            // Nothing settled, so no receipt was consumed and the bill is still open.
            let again = edge
                .settle_bill(actor(), opened.bill_id, vec![cash(165_000)], vec![])
                .await
                .expect("the still-open bill settles");
            assert_eq!(
                again.receipt_number,
                Some(1),
                "the refused attempt took no number"
            );
        });
    }

    #[test]
    fn opening_a_bill_on_an_unseated_table_is_refused() {
        pos_fakes::executor::run_ready(async {
            let edge = edge();
            let table = TableId::new(Ulid::from_u128(304));
            // A free table cannot be asked for its bill.
            let refused = edge.open_bill(actor(), table).await;
            assert!(matches!(refused, Err(super::AppError::Domain(_))));
        });
    }

    #[test]
    fn settling_an_unknown_bill_is_refused() {
        pos_fakes::executor::run_ready(async {
            let edge = edge();
            let refused = edge
                .settle_bill(
                    actor(),
                    pos_proto::ids::BillId::new(Ulid::from_u128(999)),
                    vec![cash(1)],
                    vec![],
                )
                .await;
            assert!(matches!(refused, Err(super::AppError::UnknownBill)));
        });
    }

    #[test]
    fn a_shift_opens_counts_and_closes_blind() {
        pos_fakes::executor::run_ready(async {
            let edge = edge();
            let opened = edge
                .open_shift(actor(), vnd(500_000))
                .await
                .expect("opens a shift");
            assert_eq!(opened.state, ShiftState::Open);
            // Counting reveals nothing: no expected amount, no variance yet.
            let counted = edge
                .count_shift(actor(), opened.shift_id, 500_000)
                .await
                .expect("counts");
            assert_eq!(counted.state, ShiftState::Counted);
            assert_eq!(counted.counted_amount, Some(vnd(500_000)));
            assert_eq!(counted.expected_amount, None, "the count is blind");
            assert_eq!(counted.variance, None, "the count is blind");
            // Closing reveals the expected amount (just the float, no sales) and a zero variance.
            let closed = edge
                .close_shift(actor(), opened.shift_id)
                .await
                .expect("closes");
            assert_eq!(closed.state, ShiftState::Closed);
            assert_eq!(closed.expected_amount, Some(vnd(500_000)));
            assert_eq!(closed.variance, Some(vnd(0)));
            assert!(closed.print_shift_report, "closing prints the shift report");
        });
    }

    #[test]
    fn a_shift_rolls_up_only_cash_from_settled_bills() {
        pos_fakes::executor::run_ready(async {
            let edge = edge();
            let opened = edge
                .open_shift(actor(), vnd(500_000))
                .await
                .expect("opens a shift");

            // A cash sale of 165k lands in the drawer.
            let table = TableId::new(Ulid::from_u128(400));
            edge.seat_table(actor(), table).await.expect("seats");
            edge.add_line(actor(), table, a_line()).await.expect("adds");
            let bill = edge.open_bill(actor(), table).await.expect("opens a bill");
            edge.settle_bill(actor(), bill.bill_id, vec![cash(165_000)], vec![])
                .await
                .expect("settles in cash");

            // A card sale does not: it never touches the drawer.
            let table2 = TableId::new(Ulid::from_u128(401));
            edge.seat_table(actor(), table2).await.expect("seats");
            edge.add_line(actor(), table2, a_line())
                .await
                .expect("adds");
            let bill2 = edge.open_bill(actor(), table2).await.expect("opens a bill");
            let card = Payment {
                method: PaymentMethod::Card,
                tendered: vnd(165_000),
                applied_to_bill: vnd(165_000),
            };
            edge.settle_bill(actor(), bill2.bill_id, vec![card], vec![])
                .await
                .expect("settles on card");

            // Expected drawer cash is float + cash sales only: 500k + 165k.
            edge.count_shift(actor(), opened.shift_id, 665_000)
                .await
                .expect("counts");
            let closed = edge
                .close_shift(actor(), opened.shift_id)
                .await
                .expect("closes");
            assert_eq!(closed.expected_amount, Some(vnd(665_000)));
            assert_eq!(closed.variance, Some(vnd(0)));
        });
    }

    #[test]
    fn variance_is_negative_when_the_drawer_is_short() {
        pos_fakes::executor::run_ready(async {
            let edge = edge();
            let opened = edge
                .open_shift(actor(), vnd(500_000))
                .await
                .expect("opens a shift");
            edge.count_shift(actor(), opened.shift_id, 400_000)
                .await
                .expect("counts 100k short");
            let closed = edge
                .close_shift(actor(), opened.shift_id)
                .await
                .expect("closes");
            assert_eq!(closed.variance, Some(vnd(-100_000)));
            assert!(closed.variance.expect("variance").is_negative());
        });
    }

    #[test]
    fn opening_a_second_shift_is_refused() {
        pos_fakes::executor::run_ready(async {
            let edge = edge();
            edge.open_shift(actor(), vnd(500_000))
                .await
                .expect("opens a shift");
            let refused = edge.open_shift(actor(), vnd(500_000)).await;
            assert!(matches!(refused, Err(super::AppError::ShiftAlreadyOpen)));
        });
    }

    #[test]
    fn closing_without_counting_is_refused() {
        pos_fakes::executor::run_ready(async {
            let edge = edge();
            let opened = edge
                .open_shift(actor(), vnd(500_000))
                .await
                .expect("opens a shift");
            // The close is blind: it cannot skip the count. Open -> Close is not a legal move.
            let refused = edge.close_shift(actor(), opened.shift_id).await;
            assert!(matches!(refused, Err(super::AppError::Domain(_))));
        });
    }

    #[test]
    fn counting_an_unknown_shift_is_refused() {
        pos_fakes::executor::run_ready(async {
            let edge = edge();
            let refused = edge
                .count_shift(actor(), pos_proto::ids::ShiftId::new(Ulid::from_u128(9)), 1)
                .await;
            assert!(matches!(refused, Err(super::AppError::UnknownShift)));
        });
    }

    #[test]
    fn a_new_shift_opens_after_the_previous_one_closes() {
        pos_fakes::executor::run_ready(async {
            let edge = edge();
            let first = edge
                .open_shift(actor(), vnd(500_000))
                .await
                .expect("opens the first shift");
            edge.count_shift(actor(), first.shift_id, 500_000)
                .await
                .expect("counts");
            edge.close_shift(actor(), first.shift_id)
                .await
                .expect("closes the first shift");
            // With the first shift closed, a new one may open.
            edge.open_shift(actor(), vnd(300_000))
                .await
                .expect("opens the next shift");
        });
    }
}
