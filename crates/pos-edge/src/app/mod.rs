// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The application layer: load → decide → apply → publish.
//!
//! This is the thin layer [ADR-0013](../../../../docs/adr/0013-async-strategy.md) says each binary
//! owns. For every command it: **loads** the aggregate's current state from the projection,
//! **decides** with the synchronous `pos-core` spine ([`decide_table`] and its siblings), **applies**
//! the decision by writing the wire events it maps to inside one store transaction, and — only after
//! the commit — **publishes** the change to every device over the fan-out and folds it into the
//! projection. A rolled-back transaction therefore prints nothing, tells no device, and changes no
//! screen.
//!
//! [`Edge`] is generic over the store `S`, so the same loop runs against `pos-fakes` in a test and
//! `store-sqlite` on a real machine — static dispatch, no `dyn`
//! ([ADR-0013](../../../../docs/adr/0013-async-strategy.md)). It wires the **table floor cycle** (seat,
//! clean) and the **order line** (add, fire); the bill and shift families follow the identical shape.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::sync::{Arc, Mutex, RwLock};

use pos_core::inventory::StockProjection;
use pos_proto::TableState;
use pos_proto::ids::{BillId, DeviceId, MenuItemId, OrderId, OrderLineId, ShiftId, TableId};
use pos_proto::money::Money;
use pos_proto::time::{BusinessDate, Timestamp};

use crate::auth::Lockout;
use crate::clock::SystemClock;
use crate::fanout::Fanout;
use crate::idgen::EdgeIdGenerator;
use crate::lease_state::CurrentStanding;
use crate::line_notes::LineNotes;
use crate::receipt::ReceiptAuthority;
use crate::sntp::ClockStatus;
use crate::sync_status::SyncStatus;

mod authority;
mod bills;
mod composition;
mod drawer_access;
mod error;
mod floor;
mod kitchen;
mod orders;
mod projection;
mod publish;
mod records;
mod roster;
mod session;
mod shifts;
mod views;

pub use error::{AppError, Approval};
use records::{
    BillRecord, LineRecord, LiveSnapshot, OrderOrigin, OrderSnapshot, SettleDrawer, SettledReceipt,
    ShiftRecord, StaffDecision, StaffStanding, another_till, is_till,
};
pub(crate) use records::{fee_records, fee_records_in};
pub use roster::{StaffAuth, StaffRoster, StoreIdentity};
pub use session::EdgeSession;
pub use views::{
    BillCheckView, BillView, BumpView, BuyerDetails, CashMovement, ClosedDrawers, CombinedReport,
    CounterOrderLine, CounterOrderOpened, CounterOrderView, DrawerFigures, DrawerView, DrawersView,
    FiredLine, LineDraft, LineView, LiveOrderLine, LiveOrderView, MAX_SETTLED_LISTED,
    OrderLineChoice, PreBill, ReceiptCopy, ReceiptLine, SettledBillView, ShiftReport, ShiftView,
    TableView, Takings, TillScope, TransferView, WaitingLine,
};

/// Milliseconds in a day, for turning a retention period in days into a cutoff instant. No leap
/// seconds and no daylight saving: a retention window is a duration, not a calendar walk, and being
/// an hour out twice a year on a 365-day window changes nothing anyone can observe.
const MILLISECONDS_PER_DAY: i64 = 24 * 60 * 60 * 1_000;

/// The range a store's [`EdgeSession::event_log_days`] may take
/// ([ADR-0145](../../../../docs/adr/0145-the-edge-keeps-events-until-synced-and-n-days-old.md)): the
/// one the cloud refuses a publish outside, from the `retention` node's own type
/// ([`pos_proto::retention`]). The edge ignores a value outside it that arrives anyway, keeping what
/// it had.
pub use pos_proto::retention::EVENT_LOG_DAYS;

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
    /// The channel each order came in on (roadmap **B1.2**), so a bill is taxed at the rate its own
    /// order came in on rather than at whatever [`EdgeSession::sales_channel`] happens to be
    /// store-wide. A session-wide channel is a store-wide constant, so a store serving more than one
    /// channel taxed **every** order at the session's rate: a takeaway order settled on a store
    /// whose session said dine-in got dine-in tax, and the only symptom was a wrong figure on a
    /// receipt. `sales.order.opened` has carried a `channel` since P5; nothing read it back, because
    /// there was nowhere to read it into.
    ///
    /// The value is `Option` because the channel is not always knowable: an `UNSPECIFIED` or
    /// unrecognised token from a newer sender records as `None`, and the caller falls back to the
    /// session channel rather than guessing a variant and taxing a bill at a rate nobody chose.
    ///
    /// The guest count `sales.order.opened` also carries is deliberately **not** here. It is
    /// reporting data (average check per cover) whose readers are the event log and the cloud
    /// rollups; no edge decision consults it, and a cached copy no code reads is a field that rots.
    orders: HashMap<OrderId, OrderOrigin>,
    /// The orders a member of staff has confirmed or refused
    /// ([ADR-0116](../../../../docs/adr/0116-the-qr-hold-is-derived-and-it-gates-firing.md)), folded
    /// from `sales.order.confirmed_by_staff` and `sales.order.rejected_by_staff`.
    ///
    /// This is the *only* mutable part of the hold, and it is append-only: an order enters the map
    /// once and never leaves it. The hold itself is not stored — it is derived from this map, the
    /// order's origin, and the live `qr` policy, so turning the policy off releases every held
    /// order at once instead of leaving a stored flag to be cleared one order at a time. A stored
    /// boolean is how the reported answer and the enforced answer drifted apart before.
    staff_decisions: HashMap<OrderId, StaffDecision>,
    /// Orders that ended owing nothing (`sales.order.closed`): a table released with nothing sold
    /// on it (ADR-0163), and a guest order staff refused. A closed order is not live, whatever lines
    /// it still names. A guest's order that joined the table's (`sales.table.merged`, ADR-0160
    /// item 2) ends here too: its lines are owed on the order they joined.
    closed_orders: HashSet<OrderId>,
    /// Each settled bill's receipt as the settle recorded it, for printing a copy (ADR-0164). Folded
    /// from the log like the rest, so it holds what the log holds and no more.
    settled: HashMap<BillId, SettledReceipt>,
    /// What firing has consumed, folded from each fire's `stock_movements` (§8).
    ///
    /// Until roadmap B1.1 those movements were computed by `decide_line` and **thrown away**, so
    /// the one place §8's arithmetic ran had no reader at all. They land here now.
    ///
    /// It is a running consumption total, not yet an inventory: nothing seeds it with what the
    /// store actually received, so on-hand reads negative until the flagged goods-in/stocktake
    /// slice lands. That is deliberate and safe — [`EdgeSession::item_sellable`] is the auto-86
    /// decision and it has no production caller, so nothing 86s a trading store's menu from these
    /// figures. What the fold buys today is that the consumption is real and testable rather than
    /// discarded, and that goods-in has something to seed.
    stock: StockProjection,
    bills: HashMap<BillId, BillRecord>,
    /// Every bill each order has had, oldest first, so a second bill on one order is refused
    /// (ADR-0093). See [`Projection::bill_for_order`] for why this has to be explicit.
    ///
    /// A list, because one order can have several: the parts of a split, the bill a cashier opens
    /// again after voiding the wrong one (ADR-0128). This was a single pointer to the last bill
    /// opened, and nothing moved it when that bill was voided, merged away, or settled while a
    /// sibling part still owed. That stranded a table whose bill was voided, refused every split
    /// part's settle after the first, and kept a paid order on the live list after a merge.
    order_bill_ids: HashMap<OrderId, Vec<BillId>>,
    shifts: HashMap<ShiftId, ShiftRecord>,
    /// The shift each drawer has trading or counted: the store's one drawer under `None`, a till's
    /// under its `TERMINAL` entry's id (ADR-0167 decision 2). Cash lands on a drawer's shift, and
    /// every event minted while the store's is open carries its id. A drawer leaves when its shift
    /// closes, so this holds at most one entry per drawer the store keeps.
    open_shifts: HashMap<Option<DeviceId>, ShiftId>,
    /// The lines a station has bumped (marked prepared, `kitchen.ticket.bumped`). Tracked apart from
    /// [`OrderLineState`] because a bump is orthogonal to the line's order state (a fired line is
    /// still "fired" once made); this is what lets a second KDS agree that a ticket is done.
    bumped_lines: HashSet<OrderLineId>,
    /// The items staff have marked sold out (86) at this store, until somebody restores them
    /// (`inventory.item.sold_out`, `inventory.item.restored`). An in-store override on top of the
    /// published menu's own availability: the kitchen runs out mid-service, and the console is not
    /// where anybody is standing. Folded from the log, so a restart keeps them. Bounded by the
    /// menu: only an item on it can be marked, and one a later publish drops keeps its mark,
    /// harmlessly, until it is back on the menu to be restored.
    sold_out: HashSet<MenuItemId>,
    /// Each order's lines, in id order: the index every order-scoped read walks.
    ///
    /// Until it existed those reads filtered `lines` — every line the store had sold since it was
    /// installed — so a store got slower with age, and did it holding the lock every sale takes. At
    /// thirty thousand settled orders `GET /api/orders/live` took ~60 ms and an add-line from a
    /// second device waited behind it for as long (finding F2). Written by [`Self::add_line`], and
    /// by [`Self::merge_order`], the one way a line changes order, which moves it here and on its
    /// record together.
    ///
    /// Id order rather than insertion order because every reader sorted by id — the bill's rounding
    /// residual and the receipt's row order depend on it — and the two differ after a restart whose
    /// clock stepped back.
    order_lines: HashMap<OrderId, Vec<OrderLineId>>,
    /// The orders still owing money — [`Self::open_orders`]'s rule, held exactly rather than
    /// recomputed by a scan: at least one line, no bill or a bill that has not settled, and not
    /// refused by staff.
    ///
    /// Recomputed for one order by [`Self::refresh_live`] whenever an input to that rule changes —
    /// a line added, a bill opened, a bill's state moved, a staff decision — and never simply
    /// inserted on the first line and removed on settle: voiding an order's only bill puts it
    /// back, so an order can leave the set and come back.
    live: BTreeSet<OrderId>,
    /// Paid orders without a table whose kitchen is not done: at least one fired line no station
    /// has bumped ([ADR-0161](../../../../docs/adr/0161-a-paid-order-without-a-table-stays-on-the-kitchen-board-until-it-is-done.md)).
    /// Each maps to its newest such line's fire time, which dates it for the end of the day.
    ///
    /// Kept by [`Self::refresh_live`] from the same inputs as `live`, and from a fire, a void and
    /// a bump, so no read scans for it. Bounded by [`MAX_COOKING`], and cut to the current
    /// business day by [`Edge::kitchen_orders`].
    cooking: BTreeMap<OrderId, Timestamp>,
}

/// The most paid orders the kitchen board holds waiting for a bump.
///
/// A store with no kitchen display never bumps, and nothing else would ever empty
/// [`Projection::cooking`]; past this the orders fired longest ago are dropped. A kitchen that
/// really has a thousand paid orders waiting has a bigger problem than its board.
const MAX_COOKING: usize = 1_024;

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
    /// Whether a replacement machine has taken this store
    /// ([ADR-0123](../../../../docs/adr/0123-a-superseded-box-opens-nothing-new.md)).
    ///
    /// Owned here and lent as one `Arc`, so the config-pull loop that decides it, the four commands
    /// that refuse on it and the response header that reports it are all reading the same value.
    /// `Active` until a pull says otherwise, which is every box that has never been issued a lease.
    standing: Arc<CurrentStanding>,
    /// What the status bar says about the cloud link and the outbox
    /// ([ADR-0137](../../../../docs/adr/0137-a-deep-outbox-warns-and-never-refuses.md)). Written by the
    /// outbox drain and the heartbeat, read by `GET /api/sync`.
    sync: SyncStatus,
    /// What the SNTP probe last measured of this box's own clock (roadmap-v3 PF4,
    /// [`crate::sntp`]). Written by the probe loop `main` starts, read by `GET /api/sync`; a
    /// composition that starts no loop — a test, the on-fakes example — reports it unmeasured.
    clock_status: ClockStatus,
    /// The guest notes of the lines still open, held in memory and nowhere else
    /// ([ADR-0157](../../../../docs/adr/0157-a-guest-note-lives-in-the-stores-memory-for-the-service.md)).
    ///
    /// Beside the projection rather than inside it, because the projection is rebuilt from the log
    /// and a note is precisely what the log does not hold. Locked on its own and never while the
    /// projection's lock is held.
    notes: Mutex<LineNotes>,
    /// Consecutive wrong PINs per person
    /// ([ADR-0030](../../../../docs/adr/0030-pairing-and-offline-auth.md)), shared by the sign-in route
    /// and every manager approval, so a PIN the sign-in screen would have locked cannot be guessed
    /// at an approval prompt instead. Owned here and lent to the sign-in route as one `Arc`.
    pin_lockout: Arc<Lockout>,
}

/// What [`Edge::open_inbound_order`] opened, and the one acceptance fact the caller must not work
/// out for itself.
///
/// `awaiting_staff_confirmation` is carried here rather than recomputed by the caller because it
/// depends on the store's `qr` guardrail, which the config-pull loop can replace at any moment: two
/// readers of two snapshots can disagree, and when they do, one of them has already been written to
/// a durable ledger row that every later replay of the same reference answers from.
#[derive(Debug, Clone, Copy)]
pub struct InboundOrderOpened {
    /// The order that opened.
    pub order_id: OrderId,
    /// The trading day it opened on.
    pub business_date: BusinessDate,
    /// Whether it waits for a member of staff before the kitchen sees it — the same value written
    /// to the idempotency ledger row in the order's own transaction.
    pub awaiting_staff_confirmation: bool,
}

/// What [`Edge::open_inbound_order`] needs to write the idempotency ledger row in the order's own
/// transaction ([ADR-0064](../../../../docs/adr/0064-edge-order-in.md)): the caller's key and the two
/// acceptance facts the order itself does not carry. `order_id`, `business_date` and
/// `awaiting_staff_confirmation` are the order's own and are filled in by `open_inbound_order`,
/// which answers the last of those through [`EdgeSession::holds_for_staff_confirmation`] and hands
/// it back in [`InboundOrderOpened`] so the acceptance cannot contradict the row; the
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

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::sync::Arc;

    use super::{Edge, EdgeSession, LineDraft, StoreIdentity};
    use crate::receipt::InMemoryReceipts;
    use pos_core::billing::Payment;
    use pos_core::decision::Actor;
    use pos_fakes::FakeStore;
    use pos_proto::ids::{DeviceId, EmployeeId, MenuItemId, OrderId, StoreId};
    use pos_proto::menu::MenuCatalog;
    use pos_proto::money::{CurrencyCode, Money, Ratio};
    use pos_proto::quantity::Quantity;
    use pos_proto::text::DisplayName;
    use pos_proto::ulid::Ulid;
    use pos_proto::{Open, PaymentMethod, SalesChannel};

    pub(in crate::app) fn identity() -> StoreIdentity {
        StoreIdentity::for_store(StoreId::new(Ulid::from_u128(3)))
    }

    pub(in crate::app) fn actor() -> Actor {
        Actor {
            employee_id: EmployeeId::new(Ulid::from_u128(10)),
            device_id: DeviceId::new(Ulid::from_u128(20)),
        }
    }

    pub(in crate::app) fn edge() -> Edge<FakeStore> {
        Edge::new(
            FakeStore::default(),
            identity(),
            EdgeSession::bootstrap(),
            Arc::new(InMemoryReceipts::new()),
        )
        .expect("seeds")
    }

    pub(in crate::app) fn vnd(minor: i64) -> Money {
        Money::new(CurrencyCode::VND, minor)
    }

    /// An edge whose store has published its menu, so an item's name resolves.
    ///
    /// `EdgeSession::bootstrap()` carries an **empty** catalogue on purpose — a store sells nothing
    /// to an inbound channel until the cloud publishes one — so a test that wants a name has to
    /// publish one, exactly as a store does.
    pub(in crate::app) fn edge_with_menu() -> Edge<FakeStore> {
        let menu = MenuCatalog::new().with(pos_proto::menu::MenuEntry {
            menu_item_id: MenuItemId::new(Ulid::from_u128(500)),
            display_name: DisplayName::new("Margherita"),
            display_name_translations: BTreeMap::new(),
            unit_price: vnd(150_000),
            tax_class_id: EdgeSession::standard_tax_class(),
            modifier_group_ids: Vec::new(),
            course_id: None,
            available: true,
        });
        Edge::new(
            FakeStore::default(),
            identity(),
            EdgeSession::bootstrap().with_menu(menu),
            Arc::new(InMemoryReceipts::new()),
        )
        .expect("seeds")
    }

    pub(in crate::app) fn a_line() -> LineDraft {
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
            modifier_menu_item_ids: Vec::new(),
            note_present: false,
        }
    }

    /// One already-priced line, in the shape the intake path hands to
    /// [`Edge::open_inbound_order`] — priced by the store, not by the caller.
    fn a_priced_line() -> pos_core::menu::PricedLine {
        pos_core::menu::PricedLine {
            menu_item_id: MenuItemId::new(Ulid::from_u128(500)),
            display_name: DisplayName::new("Margherita"),
            quantity: Quantity::ONE,
            unit_price: vnd(150_000),
            line_total: vnd(150_000),
            tax_class_id: EdgeSession::standard_tax_class(),
            tax_rate: Ratio::basis_points(1_000).expect("a valid rate"),
            modifier_menu_item_ids: Vec::new(),
            modifier_display_names: Vec::new(),
            repriced: false,
        }
    }

    /// Opens a tableless counter order the way a relayed takeaway order arrives: no table, no
    /// signed-in employee, the box's own device id (ADR-0064).
    pub(in crate::app) async fn a_counter_order(edge: &Edge<FakeStore>) -> OrderId {
        edge.open_inbound_order(
            actor().device_id,
            Open::from_known(SalesChannel::Takeaway),
            None,
            &[(a_priced_line(), None)],
            None,
        )
        .await
        .expect("a counter order opens with no table")
        .order_id
    }

    pub(in crate::app) fn cash(minor: i64) -> Payment {
        Payment {
            method: PaymentMethod::Cash,
            tendered: vnd(minor),
            applied_to_bill: vnd(minor),
            tip: vnd(0),
        }
    }
}
