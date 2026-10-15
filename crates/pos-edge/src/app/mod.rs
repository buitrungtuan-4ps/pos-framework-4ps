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

use pos_core::billing::{
    self, BillInput, BillLine, ClassBase, MergedFees, MergingBill, TaxComponentLine,
};
use pos_core::decision::DecisionCtx;
use pos_core::error::DomainError;
use pos_core::inventory::{StockMovement, StockProjection};
use pos_ports::TxContext;
use pos_ports::event_store::{EventQuery, EventStore};
use pos_proto::envelope::{EventEnvelope, EventPayload, EventTypeRef, RawPayload};
use pos_proto::events::{
    BillingBillMerged, BillingBillOpened, BillingBillSettled, BillingBillSplit, BillingBillVoided,
    BillingDiscountApplied, BillingFeeWaived, BillingPaymentCaptured, BillingReceiptReprinted,
    CashDrawerPaidIn, CashDrawerPaidOut, CashShiftClosed, CashShiftCounted, CashShiftOpened,
    EventType, InventoryItemRestored, InventoryItemSoldOut, KitchenTicketBumped, SalesOrderClosed,
    SalesOrderConfirmedByStaff, SalesOrderLineAdded, SalesOrderLineFired, SalesOrderLineUpdated,
    SalesOrderLineVoided, SalesOrderOpened, SalesOrderRejectedByStaff, SalesTableClosed,
    SalesTableMerged, SalesTableOpened, SalesTableTransferred,
};
use pos_proto::fees::FrozenFee;
use pos_proto::ids::{
    BillId, DeviceId, FeeId, MenuItemId, OrderId, OrderLineId, ShiftId, StationId, TableId,
};
use pos_proto::shift::DrawerModel;
// Only the `#[cfg(test)]` stock read names it; the fold itself works in `StockMovement`s.
#[cfg(test)]
use pos_proto::ids::IngredientId;
use pos_proto::money::{CurrencyCode, Money};
use pos_proto::quantity::Quantity;
use pos_proto::time::{BusinessDate, Timestamp};
use pos_proto::ulid::Ulid;
use pos_proto::{
    BillState, IdGenerator, OrderLineState, PaymentMethod, ReductionKind, SalesChannel, ShiftState,
    TableState,
};

use crate::auth::Lockout;
use crate::clock::SystemClock;
use crate::fanout::{Fanout, ServerMessage};
use crate::idgen::EdgeIdGenerator;
use crate::lease_state::CurrentStanding;
use crate::line_notes::{LineNotes, NoteText};
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
/// ([ADR-0145](../../../docs/adr/0145-the-edge-keeps-events-until-synced-and-n-days-old.md)): the
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
    /// ([ADR-0116](../../../docs/adr/0116-the-qr-hold-is-derived-and-it-gates-firing.md)), folded
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
    /// has bumped ([ADR-0161](../../../docs/adr/0161-a-paid-order-without-a-table-stays-on-the-kitchen-board-until-it-is-done.md)).
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

    /// Moves an order from the table its guests left to the one they sat down at
    /// (`sales.table.transferred`), live and on rebuild. The tables' own states are the caller's:
    /// the live command has the machine's answer, and the fold reads what that answer always is.
    fn move_order(&mut self, order_id: OrderId, from_table_id: TableId, to_table_id: TableId) {
        if self.order_for_table(from_table_id) == Some(order_id) {
            self.table_orders.remove(&from_table_id);
        }
        self.open_order(to_table_id, order_id);
        if let Some(origin) = self.orders.get_mut(&order_id) {
            origin.table_id = Some(to_table_id);
        }
    }

    /// Records where an order came from, from a live open or from the fold on rebuild.
    fn open_order_origin(
        &mut self,
        order_id: OrderId,
        channel: Option<SalesChannel>,
        table_id: Option<TableId>,
    ) {
        self.orders
            .insert(order_id, OrderOrigin { channel, table_id });
    }

    /// The channel an order came in on.
    ///
    /// `None` for an order opened before B1.2, whose log carries no `sales.order.opened`, and for
    /// one whose channel token this build does not understand — the caller falls back to the
    /// session's channel in both cases, which is exactly the old behaviour and the only answer
    /// available for a line already in the ground.
    fn order_channel(&self, order_id: OrderId) -> Option<SalesChannel> {
        self.orders.get(&order_id).and_then(|origin| origin.channel)
    }

    /// Where an order came from, if this build saw it open.
    fn order_origin(&self, order_id: OrderId) -> Option<OrderOrigin> {
        self.orders.get(&order_id).copied()
    }

    /// Records what a member of staff decided about a guest's submission (ADR-0116).
    ///
    /// A refusal also ends the order's hold on the floor, because a refused guest order is not a
    /// live order (ADR-0116 decision 5): it leaves the live list, and the table it named no longer
    /// points at it. Left pointing at it, the table would bill the refused order and take the next
    /// line onto it; left `Occupied` with no order, it would refuse every move a waiter has.
    ///
    /// The table goes back to the order it held before the guest's arrived, if one is still
    /// owing there (a guest's order takes the table over when it opens), and is freed otherwise.
    fn record_staff_decision(&mut self, order_id: OrderId, decision: StaffDecision) {
        self.staff_decisions.insert(order_id, decision);
        self.refresh_live(order_id);
        if decision == StaffDecision::Rejected
            && let Some(table_id) = self.table_for_order(order_id)
        {
            let displaced = self.live.iter().rev().copied().find(|other| {
                self.orders
                    .get(other)
                    .is_some_and(|origin| origin.table_id == Some(table_id))
            });
            if let Some(other) = displaced {
                self.open_order(table_id, other);
                let state = if self.latest_bill_in(other, BillState::Open).is_some() {
                    TableState::AwaitingPayment
                } else {
                    TableState::Occupied
                };
                self.set_table(table_id, state);
            } else {
                self.table_orders.remove(&table_id);
                if self.table_state(table_id) == TableState::Occupied {
                    self.set_table(table_id, TableState::Free);
                }
            }
        }
    }

    /// Seats a guest's own order at the table it names, as a QR order does in place of a waiter's
    /// seat. Live and on rebuild through this one method: a guest order's log carries no
    /// `sales.table.opened`, only the table on its `sales.order.opened`, and a rebuild that did not
    /// read it there brought the table back free with the order stranded off it.
    fn seat_guest_order(&mut self, table_id: TableId, order_id: OrderId) {
        self.set_table(table_id, TableState::Occupied);
        self.open_order(table_id, order_id);
    }

    /// What staff decided about an order, or `None` while it is still theirs to decide.
    fn staff_decision(&self, order_id: OrderId) -> Option<StaffDecision> {
        self.staff_decisions.get(&order_id).copied()
    }

    /// Whether this order is a guest submission still waiting on staff, under `session`'s live
    /// policy (ADR-0116).
    ///
    /// Derived, never stored: origin from the log, decision from the log, policy from the current
    /// session. An order this build never saw open has no origin and is therefore not held — the
    /// safe answer, because refusing to fire an order whose provenance is unknown would strand a
    /// pre-upgrade order nobody can release. Nor is an order that has ended: there is nothing left
    /// on it to confirm, and a guest's order that joined the table's while nobody was asked would
    /// otherwise wait, empty, from the moment a store turned the hold on.
    fn awaits_staff_confirmation(&self, order_id: OrderId, session: &EdgeSession) -> bool {
        self.staff_decision(order_id).is_none()
            && !self.closed_orders.contains(&order_id)
            && self.order_origin(order_id).is_some_and(|origin| {
                session.holds_for_staff_confirmation(origin.channel, origin.table_id)
            })
    }

    /// Where an order stands with staff, for the fire path (ADR-0116).
    ///
    /// Three answers rather than two, because "not waiting" is ambiguous in the one way that
    /// matters: a confirmed order and a refused order are both decided, and only one of them may
    /// reach the kitchen.
    fn staff_standing(&self, order_id: OrderId, session: &EdgeSession) -> StaffStanding {
        match self.staff_decision(order_id) {
            Some(StaffDecision::Rejected) => StaffStanding::Refused,
            Some(StaffDecision::Confirmed) => StaffStanding::Fireable,
            None => {
                if self.awaits_staff_confirmation(order_id, session) {
                    StaffStanding::Waiting
                } else {
                    StaffStanding::Fireable
                }
            }
        }
    }

    /// Every order still waiting on staff, oldest id first (ULIDs sort by mint time, so this is
    /// arrival order without storing a timestamp).
    fn orders_awaiting_staff_confirmation(&self, session: &EdgeSession) -> Vec<OrderId> {
        let mut held: Vec<OrderId> = self
            .orders
            .keys()
            .copied()
            .filter(|order_id| self.awaits_staff_confirmation(*order_id, session))
            .collect();
        held.sort_unstable();
        held
    }

    fn add_line(&mut self, line_id: OrderLineId, record: LineRecord) {
        let order_id = record.order_id;
        // Indexed only when the line is new, or back on the order it was added to: a rebuild can run
        // over a projection that already holds it, and a second copy of an id would sell the line
        // twice on the bill. A line a merge has moved since is put back on its own order, as its
        // event says, and the merge's own fold moves it again (`sales.table.merged`).
        let previous = self
            .lines
            .insert(line_id, record)
            .map(|previous| previous.order_id);
        if previous != Some(order_id) {
            if let Some(previous) = previous
                && let Some(ids) = self.order_lines.get_mut(&previous)
            {
                ids.retain(|id| *id != line_id);
            }
            let ids = self.order_lines.entry(order_id).or_default();
            if let Err(at) = ids.binary_search(&line_id) {
                ids.insert(at, line_id);
            }
            if let Some(previous) = previous {
                self.refresh_live(previous);
            }
        }
        self.refresh_live(order_id);
    }

    /// Ends an order owing nothing (`sales.order.closed`), live and replayed alike.
    ///
    /// A table still pointing at the order lets go of it, as a refused guest order's table does.
    /// Left pointing, a released table would take the next line sent to it onto an order that has
    /// ended: on no screen, on no bill, and never charged.
    fn close_order(&mut self, order_id: OrderId) {
        self.closed_orders.insert(order_id);
        if let Some(table_id) = self.table_for_order(order_id) {
            self.table_orders.remove(&table_id);
        }
        self.refresh_live(order_id);
    }

    /// Folds an order that arrived from outside the store ([`Edge::open_inbound_order`]), in the
    /// order its events are in, as the replay folds them: seated, then its lines, then joined to
    /// `joins` where it joined the table's order, which gives the table back to that order.
    ///
    /// A QR order names a table, so the floor shows it occupied and staff can open the bill; a
    /// delivery or public-API order has none. The channel is the one the order actually came in
    /// on, so the bill is taxed at its rate rather than the session's (B1.2), and is recorded for a
    /// tableless order too — a delivery order has no table and still has a channel.
    fn fold_inbound_order(
        &mut self,
        order_id: OrderId,
        (channel, table_id): (Option<SalesChannel>, Option<TableId>),
        lines: Vec<(OrderLineId, LineRecord)>,
        joins: Option<OrderId>,
    ) {
        if let Some(table_id) = table_id {
            self.seat_guest_order(table_id, order_id);
        }
        self.open_order_origin(order_id, channel, table_id);
        for (line_id, record) in lines {
            self.add_line(line_id, record);
        }
        if let Some(target_order_id) = joins {
            self.merge_order(target_order_id, order_id);
        }
    }

    /// Folds one order into another (`sales.table.merged`), live and replayed alike: every line of
    /// `merged` moves onto `target`, a table that pointed at `merged` points at `target`, and
    /// `merged` has ended.
    ///
    /// What a guest's QR order does at a table whose order it joins (ADR-0160 item 2), as it
    /// arrives or when staff confirm it: from then on its lines are the table's order's, fired,
    /// billed and voided there like any line added to it. Each keeps its own id and what it was
    /// added with, and its `sales.order_line.added` still names the order it was ordered on. The
    /// folded order has no line left and takes none, waits for nobody, and is off every list.
    fn merge_order(&mut self, target: OrderId, merged: OrderId) {
        if target == merged {
            return;
        }
        let moved = self.order_lines.remove(&merged).unwrap_or_default();
        for line_id in &moved {
            if let Some(record) = self.lines.get_mut(line_id) {
                record.order_id = target;
            }
        }
        let ids = self.order_lines.entry(target).or_default();
        for line_id in moved {
            if let Err(at) = ids.binary_search(&line_id) {
                ids.insert(at, line_id);
            }
        }
        if let Some(table_id) = self.table_for_order(merged) {
            self.open_order(table_id, target);
        }
        self.closed_orders.insert(merged);
        self.refresh_live(merged);
        self.refresh_live(target);
    }

    /// The order at `table_id` a guest's lines may join (ADR-0160 item 2), other than `excluding`:
    /// the table's own order, or failing it the newest other order still owing there. `None` when
    /// none of them can take a line ([`Self::takes_joining_lines`]), and the guest's order is then
    /// its own order, as before.
    fn joinable_order_at(
        &self,
        table_id: TableId,
        excluding: Option<OrderId>,
        session: &EdgeSession,
    ) -> Option<OrderId> {
        let at_table = |order_id: &OrderId| {
            self.order_origin(*order_id)
                .is_some_and(|origin| origin.table_id == Some(table_id))
        };
        self.order_for_table(table_id)
            .into_iter()
            .chain(self.live.iter().rev().copied().filter(at_table))
            .find(|order_id| {
                Some(*order_id) != excluding && self.takes_joining_lines(*order_id, session)
            })
    }

    /// Whether an order can take a guest's joining lines: it has not ended, it is not a guest's
    /// order staff refused or have still to decide on, and it is on no bill. A line rung after a
    /// bill opens is on no bill (ADR-0128 decision 9), which is why the floor refuses one then, and
    /// a paid order's bill is behind it.
    fn takes_joining_lines(&self, order_id: OrderId, session: &EdgeSession) -> bool {
        !self.closed_orders.contains(&order_id)
            && self.staff_standing(order_id, session) == StaffStanding::Fireable
            && self.bill_for_order(order_id).is_none()
    }

    /// Recomputes whether `order_id` is still owing money, from the rule's own inputs.
    fn refresh_live(&mut self, order_id: OrderId) {
        let has_lines = self
            .order_lines
            .get(&order_id)
            .is_some_and(|ids| !ids.is_empty());
        let settled = self
            .bill_for_order(order_id)
            .and_then(|bill_id| self.bills.get(&bill_id))
            .is_some_and(|bill| bill.state == BillState::Settled);
        let refused = self.staff_decision(order_id) == Some(StaffDecision::Rejected);
        let closed = self.closed_orders.contains(&order_id);
        if has_lines && !settled && !refused && !closed {
            self.live.insert(order_id);
        } else {
            self.live.remove(&order_id);
        }
        // Paid, and served at no table: its food is still coming while a line waits for a bump.
        // "At a table" is where the order opened as well as where a table points now: a table
        // seated again points at its new guests, and the old order is still a table's order.
        let at_a_table = self.table_for_order(order_id).is_some()
            || self
                .order_origin(order_id)
                .is_some_and(|origin| origin.table_id.is_some());
        let waiting = if settled && !refused && !at_a_table {
            self.newest_unbumped_fire(order_id)
        } else {
            None
        };
        match waiting {
            Some(fired) => {
                self.cooking.insert(order_id, fired);
                self.bound_cooking();
            }
            None => {
                self.cooking.remove(&order_id);
            }
        }
    }

    /// The newest fire time among an order's fired lines that no station has bumped.
    fn newest_unbumped_fire(&self, order_id: OrderId) -> Option<Timestamp> {
        self.line_ids_for_order(order_id)
            .iter()
            .filter(|line_id| !self.bumped_lines.contains(line_id))
            .filter_map(|line_id| self.lines.get(line_id))
            .filter(|record| record.state == OrderLineState::Fired)
            .filter_map(|record| record.fired_time)
            .max()
    }

    /// Keeps [`Self::cooking`] within [`MAX_COOKING`] by dropping the orders fired longest ago.
    fn bound_cooking(&mut self) {
        while self.cooking.len() > MAX_COOKING {
            let oldest = self
                .cooking
                .iter()
                .min_by_key(|(_, fired)| **fired)
                .map(|(order_id, _)| *order_id);
            match oldest {
                Some(order_id) => {
                    self.cooking.remove(&order_id);
                }
                None => break,
            }
        }
    }

    /// Whether a line is one the kitchen board still shows for an order that no longer owes:
    /// fired, not bumped, on an order in [`Self::cooking`].
    fn still_cooking(&self, order_line_id: OrderLineId, record: &LineRecord) -> bool {
        record.state == OrderLineState::Fired
            && !self.bumped_lines.contains(&order_line_id)
            && self.cooking.contains_key(&record.order_id)
    }

    /// The ids of an order's lines, in id order, from the index.
    fn line_ids_for_order(&self, order_id: OrderId) -> &[OrderLineId] {
        self.order_lines.get(&order_id).map_or(&[], Vec::as_slice)
    }

    /// An order's lines that satisfy `keep`, each with its id, in id order.
    fn lines_where(
        &self,
        order_id: OrderId,
        keep: impl Fn(&LineRecord) -> bool,
    ) -> Vec<(OrderLineId, LineRecord)> {
        self.line_ids_for_order(order_id)
            .iter()
            .filter_map(|id| self.lines.get(id).map(|record| (*id, record)))
            .filter(|(_, record)| keep(record))
            .map(|(id, record)| (id, record.clone()))
            .collect()
    }

    fn line(&self, line_id: OrderLineId) -> Option<LineRecord> {
        self.lines.get(&line_id).cloned()
    }

    /// Folds one fire's consumption into the stock projection.
    ///
    /// A movement whose running total would leave `i64` is dropped with a warning rather than
    /// failing the fire: the sale is already committed and durable at this point, and refusing to
    /// update a cache is not a reason to tell the kitchen a line did not fire.
    fn consume(&mut self, movements: &[StockMovement]) {
        for movement in movements {
            if let Err(error) = self.stock.apply(movement) {
                tracing::warn!(%error, "a stock movement overflowed the projection; not folded");
            }
        }
    }

    /// The on-hand figure for an ingredient — negative until goods-in seeds the projection.
    ///
    /// `#[cfg(test)]` because the *fold* has a production caller (every fire) and the *read* does
    /// not yet: `EdgeSession::item_sellable` is the auto-86 decision and nothing calls it outside
    /// tests. Shipping a reader with no reader would be dead code; the goods-in slice that needs it
    /// removes this attribute rather than adding the method.
    #[cfg(test)]
    fn on_hand(&self, ingredient: IngredientId) -> Quantity {
        self.stock.on_hand(ingredient)
    }

    fn set_line_state(&mut self, line_id: OrderLineId, state: OrderLineState) {
        let Some(record) = self.lines.get_mut(&line_id) else {
            return;
        };
        record.state = state;
        let order_id = record.order_id;
        // A fire or a void can start or end a paid order's wait at the kitchen (ADR-0161).
        self.refresh_live(order_id);
    }

    /// Records when a line went to the kitchen, beside the state change that sent it.
    ///
    /// Its own method rather than a second argument to [`Self::set_line_state`], because a void and
    /// a hold move a line's state and have no fire time to record — and because **every** path that
    /// fires has to call both. There are three: the single-line fire, the batch fire behind
    /// "send to kitchen", and the fold that rebuilds a projection from the log. A fourth that set
    /// the state and forgot the time would give a screen a ticket it could not age, which is the
    /// shape of the bug this pair replaced.
    ///
    /// The value is `SalesOrderLineFired::fire_time` — the domain's own field, minted once per
    /// command — so the live write and the replay of that same command agree by construction rather
    /// than by two clocks happening to match.
    fn set_fired_time(
        &mut self,
        line_id: OrderLineId,
        fired_time: Timestamp,
        station_id: StationId,
    ) {
        let Some(record) = self.lines.get_mut(&line_id) else {
            return;
        };
        record.fired_time = Some(fired_time);
        record.station_id = Some(station_id);
        let order_id = record.order_id;
        self.refresh_live(order_id);
    }

    /// Both halves of a fire, for the replay.
    ///
    /// The two live paths set the state from the decision they just took, so they call
    /// [`Self::set_line_state`] and [`Self::set_fired_time`] in turn. The fold has no decision — the
    /// event *is* the fire — so it says so in one call, which also keeps [`Self::fold`] inside the
    /// hundred lines clippy allows it. That limit is worth keeping: the fold is one match over every
    /// event type this edge knows, and it earns its length one arm at a time.
    fn fold_fired(&mut self, line_id: OrderLineId, fired_time: Timestamp, station_id: StationId) {
        self.set_line_state(line_id, OrderLineState::Fired);
        self.set_fired_time(line_id, fired_time, station_id);
    }

    /// Marks an item sold out, or restores it — from a live mark and from the fold on rebuild.
    fn set_sold_out(&mut self, menu_item_id: MenuItemId, sold_out: bool) {
        if sold_out {
            self.sold_out.insert(menu_item_id);
        } else {
            self.sold_out.remove(&menu_item_id);
        }
    }

    /// Marks lines bumped (prepared) — from a live `bump_ticket` and from the fold on rebuild.
    fn mark_bumped(&mut self, order_line_ids: &[OrderLineId]) {
        self.bumped_lines.extend(order_line_ids.iter().copied());
        // A bump can end a paid order's wait at the kitchen (ADR-0161).
        let orders: BTreeSet<OrderId> = order_line_ids
            .iter()
            .filter_map(|line_id| self.lines.get(line_id).map(|record| record.order_id))
            .collect();
        for order_id in orders {
            self.refresh_live(order_id);
        }
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
        self.identified_lines_for_order(order_id)
            .into_iter()
            .map(|(_, record)| record)
            .collect()
    }

    /// The same lines, still carrying the id each one is addressed by.
    ///
    /// A screen that has to *act* on a line — fire it, void it, bump it — needs the id, and a
    /// screen rebuilding after a restart has no other way to learn one: the ids reach a device on
    /// the fan-out, and a device that was not running when the line was added never saw it.
    fn identified_lines_for_order(&self, order_id: OrderId) -> Vec<(OrderLineId, LineRecord)> {
        self.lines_where(order_id, |_| true)
    }

    /// Records a bill and indexes it by the order it bills.
    fn open_bill_record(&mut self, bill_id: BillId, record: BillRecord) {
        let order_id = record.order_id;
        // Indexed only when new, as a line is: a rebuild can fold a bill the projection holds.
        let ids = self.order_bill_ids.entry(order_id).or_default();
        if !ids.contains(&bill_id) {
            ids.push(bill_id);
        }
        self.bills.insert(bill_id, record);
        self.refresh_live(order_id);
    }

    /// The newest of an order's bills that is in `state`.
    fn latest_bill_in(&self, order_id: OrderId, state: BillState) -> Option<BillId> {
        self.order_bill_ids
            .get(&order_id)?
            .iter()
            .rev()
            .copied()
            .find(|bill_id| {
                self.bills
                    .get(bill_id)
                    .is_some_and(|bill| bill.state == state)
            })
    }

    /// The bill an order is being charged on: its newest open bill, or if none is open its newest
    /// settled one. `None` when it never had a bill, or every bill it had was voided or folded into
    /// another, which leaves it owing and free to bill again.
    ///
    /// This is what keeps one order to one bill now that a bill is keyed on an order rather than a
    /// table ([ADR-0093](../../../docs/adr/0093-bill-keyed-on-order.md)). It used to hold by
    /// accident: a table can only be `Occupied` once, so `decide_table(RequestBill)` refused the
    /// second request on its way through, and nothing had to say so. An order key has no state
    /// machine behind it, and dropping the rule silently would be expensive — two open bills on one
    /// order means two receipt numbers and a guest charged twice for one meal.
    ///
    /// Open before settled, so an order with one split part paid and another still owing is
    /// still owing, whichever part was opened last.
    fn bill_for_order(&self, order_id: OrderId) -> Option<BillId> {
        self.latest_bill_in(order_id, BillState::Open)
            .or_else(|| self.latest_bill_in(order_id, BillState::Settled))
    }

    /// Every bill on an order that is still open, oldest first: one for an ordinary table, one per
    /// unpaid part once its bill has been split (ADR-0128).
    fn open_bills_for_order(&self, order_id: OrderId) -> Vec<BillId> {
        self.order_bill_ids
            .get(&order_id)
            .map_or_else(Vec::new, |ids| {
                ids.iter()
                    .copied()
                    .filter(|bill_id| {
                        self.bills
                            .get(bill_id)
                            .is_some_and(|bill| bill.state == BillState::Open)
                    })
                    .collect()
            })
    }

    /// Keeps what a settle recorded, live and replayed alike, so the receipt can be copied.
    fn record_settled(&mut self, bill_id: BillId, receipt: SettledReceipt) {
        self.settled.insert(bill_id, receipt);
    }

    /// Counts a copy (`billing.receipt.reprinted`). The highest number seen wins, so a replay that
    /// meets the copies in any order ends where the live box did.
    fn record_copy(&mut self, bill_id: BillId, copy_number: u32) {
        if let Some(receipt) = self.settled.get_mut(&bill_id) {
            receipt.copies = receipt.copies.max(copy_number);
        }
    }

    /// What the bills settled on `business_date` came to and how many there were, in `currency`.
    /// A bill the projection no longer holds as settled took nothing, so it is not counted.
    ///
    /// Every bill the day settled, and not the most recent [`MAX_SETTLED_LISTED`]: this fold keeps
    /// each settle the log holds, and a busy day settles more bills than the till lists.
    fn takings_on(
        &self,
        business_date: BusinessDate,
        currency: CurrencyCode,
    ) -> Result<(Money, u32), DomainError> {
        let mut taken = Money::zero(currency);
        let mut count: u32 = 0;
        for (bill_id, receipt) in &self.settled {
            let still_settled = self
                .bills
                .get(bill_id)
                .is_some_and(|bill| bill.state == BillState::Settled);
            if receipt.business_date == business_date && still_settled {
                taken = taken.checked_add(receipt.total_due)?;
                count = count.saturating_add(1);
            }
        }
        Ok((taken, count))
    }

    /// The receipts settled on `business_date`, newest first, at most `limit` of them.
    fn settled_on(
        &self,
        business_date: BusinessDate,
        limit: usize,
    ) -> Vec<(BillId, SettledReceipt)> {
        let mut receipts: Vec<(BillId, SettledReceipt)> = self
            .settled
            .iter()
            .filter(|(_, receipt)| receipt.business_date == business_date)
            .map(|(bill_id, receipt)| (*bill_id, receipt.clone()))
            .collect();
        receipts.sort_unstable_by(|(_, a), (_, b)| {
            b.settle_time
                .cmp(&a.settle_time)
                .then(b.receipt_number.cmp(&a.receipt_number))
        });
        receipts.truncate(limit);
        receipts
    }

    /// Settles a bill, and moves its table on when nothing else on the order still owes.
    ///
    /// Only then: the parts of a split share one table, and moving it to `NeedsCleaning` on the
    /// first part's settle left every other part refused by the table machine, with no way to take
    /// the rest of the money. Decided here, under the projection's lock after the commit, so two
    /// tills settling the last two parts at once cannot each see the other still open.
    fn settle_bill_record(&mut self, bill_id: BillId) {
        self.set_bill_state(bill_id, BillState::Settled);
        let Some((order_id, Some(table_id))) = self
            .bills
            .get(&bill_id)
            .map(|bill| (bill.order_id, bill.table_id))
        else {
            return;
        };
        if self.latest_bill_in(order_id, BillState::Open).is_none() {
            self.set_table(table_id, TableState::NeedsCleaning);
        }
    }

    /// Voids a bill, and gives its table back when nothing else on the order still owes.
    ///
    /// A void cancels the bill, not the meal: the lines are still there, and a cashier who voided
    /// the wrong bill opens another on the same table. So the table leaves `AwaitingPayment`: to
    /// `Occupied` when nothing was paid, or to `NeedsCleaning` when a sibling part was, because
    /// that meal is finished and the voided part's lines were forgiven with it. Before this, the
    /// table stayed `AwaitingPayment` for good, a state only a settle leaves, and a voided bill
    /// cannot settle.
    fn void_bill_record(&mut self, bill_id: BillId) {
        self.set_bill_state(bill_id, BillState::Voided);
        let Some((order_id, Some(table_id))) = self
            .bills
            .get(&bill_id)
            .map(|bill| (bill.order_id, bill.table_id))
        else {
            return;
        };
        if self.table_state(table_id) != TableState::AwaitingPayment
            || self.latest_bill_in(order_id, BillState::Open).is_some()
        {
            return;
        }
        let next = if self.latest_bill_in(order_id, BillState::Settled).is_some() {
            TableState::NeedsCleaning
        } else {
            TableState::Occupied
        };
        self.set_table(table_id, next);
    }

    /// Every **counter** order still owing money, in id order (so the counter list is stable across
    /// refreshes rather than reshuffling with the hash map).
    ///
    /// A counter order is one with lines and no table: a relayed marketplace order, the public API,
    /// a takeaway guest ([ADR-0064](../../../docs/adr/0064-edge-order-in.md)). Floor orders are
    /// excluded because the floor screen already shows them — listing a table's order here would
    /// give staff two places to charge one meal.
    ///
    /// "Still owing" means no bill, or a bill that has not settled. A settled order is finished; a
    /// bill already open is *not* finished and stays on the list, because the cashier who opened it
    /// still has to take the money and the screen must be able to resume rather than start again.
    fn open_counter_orders(&self) -> Vec<OrderId> {
        let on_a_table: HashSet<OrderId> = self.table_orders.values().copied().collect();
        self.open_orders()
            .into_iter()
            .filter(|order_id| !on_a_table.contains(order_id))
            .collect()
    }

    /// Every order still owing money, table **and** counter, in id order.
    ///
    /// The set a device has to be able to rebuild from nothing: a till that reloads, or a kitchen
    /// display that is switched on mid-service, learns lines only from the fan-out, and the fan-out
    /// carries what happens *next*, never what already happened. Without this read such a screen
    /// draws an empty order and an empty board while the food exists.
    ///
    /// "Still owing" is [`Self::open_counter_orders`]'s rule, which this one now supplies: no bill,
    /// or a bill that has not settled. A settled order is finished and drops out, which is what
    /// bounds this list — it is the store's live trading, not a log.
    ///
    /// Read from [`Self::live`], which holds exactly this rule, rather than by scanning every line
    /// the store has ever sold (F2).
    fn open_orders(&self) -> Vec<OrderId> {
        self.live.iter().copied().collect()
    }

    /// When the oldest business still open began, in milliseconds since the epoch, or `None` when
    /// nothing is open (ADR-0145): an order still owing, an open bill, a table that is not free, a
    /// guest order awaiting staff, the open shift.
    ///
    /// Read from the ids, not from stored times: every one of these is a ULID minted when it
    /// opened, so its high bits are the moment it began, and every event that belongs to it came
    /// after. Retention deletes nothing from that moment on, so a rebuild still finds everything the
    /// open work needs.
    fn oldest_open_work(&self, session: &EdgeSession) -> Option<u64> {
        let live = self.live.first().map(|order_id| order_id.as_ulid());
        let bills = self
            .bills
            .iter()
            .filter(|(_, bill)| bill.state == BillState::Open)
            .map(|(bill_id, _)| bill_id.as_ulid());
        let tables = self
            .tables
            .iter()
            .filter(|(_, state)| **state != TableState::Free)
            .filter_map(|(table_id, _)| self.table_orders.get(table_id))
            .map(|order_id| order_id.as_ulid());
        let held = self
            .orders_awaiting_staff_confirmation(session)
            .first()
            .map(|order_id| order_id.as_ulid());
        let shifts = self.open_shifts.values().map(|shift_id| shift_id.as_ulid());
        live.into_iter()
            .chain(bills)
            .chain(tables)
            .chain(held)
            .chain(shifts)
            .map(Ulid::timestamp_ms)
            .min()
    }

    /// The lines on an order that have not been sent yet, each with the id it is sent by, in id
    /// order — so a send goes out in the order the operator typed it.
    ///
    /// The set one tap on "send" acts on. Kept as a query rather than filtered at the call site
    /// because "unfired" is a projection fact: [`OrderLineState::Added`] is the only state a fire is
    /// legal from, and the decision refuses the rest anyway.
    fn unfired_lines_for_order(&self, order_id: OrderId) -> Vec<(OrderLineId, LineRecord)> {
        self.lines_where(order_id, |record| record.state == OrderLineState::Added)
    }

    /// Applies a new quantity and extended total to a line the order still holds.
    ///
    /// Silent on a line this projection does not know, like every other setter here: a replay that
    /// reaches an update before its add is a replay out of order, and refusing would stop a box
    /// trading over a line that will exist a moment later.
    fn set_line_quantity(
        &mut self,
        order_line_id: OrderLineId,
        quantity: Quantity,
        line_total: Money,
    ) {
        if let Some(record) = self.lines.get_mut(&order_line_id) {
            record.quantity = quantity;
            record.line_total = line_total;
        }
    }

    fn bill(&self, bill_id: BillId) -> Option<BillRecord> {
        self.bills.get(&bill_id).cloned()
    }

    /// The unvoided lines of `order_id`, in id order — what opening a bill covers.
    ///
    /// Unvoided, because a voided line is owed nothing and taxed nothing; in id order, because that
    /// is the order the merge semantics fix (ADR-0029) and a partition has to be comparable against
    /// something stable.
    fn sold_line_ids(&self, order_id: OrderId) -> Vec<OrderLineId> {
        self.line_ids_for_order(order_id)
            .iter()
            .copied()
            .filter(|id| {
                self.lines
                    .get(id)
                    .is_some_and(|record| record.state != OrderLineState::Voided)
            })
            .collect()
    }

    /// The lines a bill covers: the ones it names, or — for a bill that names none, opened before
    /// ADR-0128 recorded them — every unvoided line on its order, which is what such a bill meant.
    fn covered_lines(&self, bill: &BillRecord) -> Vec<OrderLineId> {
        if bill.order_line_ids.is_empty() {
            self.sold_line_ids(bill.order_id)
        } else {
            bill.order_line_ids.clone()
        }
    }

    /// Records a reduction against a bill. Additive, because two discounts on one bill are two
    /// facts rather than a correction of the first.
    ///
    /// A kind this build does not know — `VOID`, or a token from a newer sender — moves nothing.
    /// The alternative is to guess which column it belongs in, and guessing wrong understates what
    /// a guest owes.
    fn add_reduction(
        &mut self,
        bill_id: BillId,
        kind: ReductionKind,
        amount: Money,
    ) -> Result<(), DomainError> {
        if let Some(record) = self.bills.get_mut(&bill_id) {
            let slot = match kind {
                ReductionKind::Discount => &mut record.discount,
                ReductionKind::Comp => &mut record.comp,
                ReductionKind::Void | ReductionKind::Unspecified => return Ok(()),
            };
            *slot = Some(match *slot {
                Some(running) => running.checked_add(amount)?,
                None => amount,
            });
        }
        Ok(())
    }

    /// Gives each part of a split its source's wholes and waived fees (ADR-0159 decisions 5 and
    /// 6), after each part's own `billing.bill.opened`: merging the parts back puts their shares
    /// together again, and a fee waived on the bill stays waived on every part.
    fn inherit_fees(&mut self, source: BillId, parts: &[BillId]) {
        let Some((wholes, waived)) = self
            .bills
            .get(&source)
            .map(|bill| (bill.fee_wholes.clone(), bill.waived_fee_ids.clone()))
        else {
            return;
        };
        for part in parts {
            if let Some(record) = self.bills.get_mut(part) {
                record.fee_wholes.clone_from(&wholes);
                record.waived_fee_ids.clone_from(&waived);
            }
        }
    }

    /// Records a fee waived on a bill (ADR-0159 decision 5), once: a second waive of the same fee
    /// is refused before it is written, and a replay that meets one anyway changes nothing.
    fn waive_fee_record(&mut self, bill_id: BillId, fee_id: FeeId) {
        if let Some(record) = self.bills.get_mut(&bill_id)
            && !record.waived_fee_ids.contains(&fee_id)
        {
            record.waived_fee_ids.push(fee_id);
        }
    }

    /// The fee rules and wholes `target` holds once `absorbed` are merged into it, in that order
    /// (ADR-0159 decision 6), from what each holds now; `None` when the target is unknown.
    fn merged_fees(
        &self,
        target: BillId,
        absorbed: &[BillId],
    ) -> Result<Option<MergedFees>, DomainError> {
        let Some(holder) = self.bills.get(&target) else {
            return Ok(None);
        };
        let absorbed: Vec<MergingBill<'_>> = absorbed
            .iter()
            .filter_map(|bill_id| self.bills.get(bill_id))
            .map(BillRecord::merging)
            .collect();
        Ok(Some(billing::merge_fee_rules(holder.merging(), &absorbed)?))
    }

    /// Records what a merged bill now holds.
    fn set_bill_fees(&mut self, bill_id: BillId, fees: MergedFees) {
        if let Some(record) = self.bills.get_mut(&bill_id) {
            record.fee_rules = fees.fee_rules;
            record.fee_wholes = fees.fee_wholes;
            record.waived_fee_ids = fees.waived_fee_ids;
        }
    }

    fn set_bill_state(&mut self, bill_id: BillId, state: BillState) {
        let Some(record) = self.bills.get_mut(&bill_id) else {
            return;
        };
        record.state = state;
        let order_id = record.order_id;
        self.refresh_live(order_id);
    }

    /// Replaces what a bill covers — the target of a merge, taking the absorbed bills' lines.
    fn set_bill_lines(&mut self, bill_id: BillId, order_line_ids: Vec<OrderLineId>) {
        if let Some(record) = self.bills.get_mut(&bill_id) {
            record.order_line_ids = order_line_ids;
        }
    }

    fn open_shift_record(&mut self, shift_id: ShiftId, record: ShiftRecord) {
        self.open_shifts.insert(record.till, shift_id);
        self.shifts.insert(shift_id, record);
    }

    /// Records that `shift_id`'s drawer was found open past its day's end on `today`, answering
    /// whether that is the first time today: one warning a drawer a day.
    fn warn_day_ended(&mut self, shift_id: ShiftId, today: BusinessDate) -> bool {
        match self.shifts.get_mut(&shift_id) {
            Some(record) if record.day_end_warned != Some(today) => {
                record.day_end_warned = Some(today);
                true
            }
            _ => false,
        }
    }

    /// The shift `till`'s drawer has trading or counted, `None` for the store's drawer.
    fn open_shift_of(&self, till: Option<DeviceId>) -> Option<ShiftId> {
        self.open_shifts.get(&till).copied()
    }

    /// The model the open shifts were opened under, or `None` when no drawer is open: a till's
    /// drawer is only ever opened under a drawer per till, and the store's under one drawer. Were
    /// both ever open, a till's decides, because that model lists every open drawer.
    fn open_model(&self) -> Option<DrawerModel> {
        if self.open_shifts.keys().any(Option::is_some) {
            Some(DrawerModel::PerTerminal)
        } else if self.open_shifts.is_empty() {
            None
        } else {
            Some(DrawerModel::PerStore)
        }
    }

    fn shift(&self, shift_id: ShiftId) -> Option<ShiftRecord> {
        self.shifts.get(&shift_id).cloned()
    }

    fn record_shift_count(&mut self, shift_id: ShiftId, counted: Money, state: ShiftState) {
        if let Some(record) = self.shifts.get_mut(&shift_id) {
            record.counted = Some(counted);
            record.state = state;
        }
    }

    fn close_shift_record(&mut self, shift_id: ShiftId) {
        let Some(record) = self.shifts.get_mut(&shift_id) else {
            return;
        };
        record.state = ShiftState::Closed;
        let till = record.till;
        if self.open_shift_of(till) == Some(shift_id) {
            self.open_shifts.remove(&till);
        }
    }

    /// Adds a paid in or a paid out to `shift_id`'s record (ADR-0165 decision 2).
    fn record_movement(
        &mut self,
        shift_id: ShiftId,
        movement: CashMovement,
        amount: Money,
    ) -> Result<(), DomainError> {
        if let Some(record) = self.shifts.get_mut(&shift_id) {
            match movement {
                CashMovement::PaidIn => record.paid_in = record.paid_in.checked_add(amount)?,
                CashMovement::PaidOut => record.paid_out = record.paid_out.checked_add(amount)?,
            }
        }
        Ok(())
    }

    /// Folds a settle into the projection, live and replayed alike: the bill settles, its receipt is
    /// kept to be copied (ADR-0164), and its tax by component joins `shift`, the shift its envelope
    /// is stamped with ([`Self::collect_tax_components`]).
    fn fold_settle(
        &mut self,
        settled: &BillingBillSettled,
        shift: Option<ShiftId>,
        business_date: BusinessDate,
        settle_time: Timestamp,
    ) -> Result<(), DomainError> {
        self.settle_bill_record(settled.bill_id);
        self.record_settled(
            settled.bill_id,
            SettledReceipt::of(settled, business_date, settle_time),
        );
        self.collect_tax_components(shift, settled)
    }

    /// Adds a settled bill's tax by component to `shift_id`'s record, if the settle was stamped with
    /// a shift: each component its tax lines recorded, to the entry of its name and rate (ADR-0168
    /// decision 5). Fee tax is inside the tax lines, so the fee lines' own components are not
    /// added.
    fn collect_tax_components(
        &mut self,
        shift_id: Option<ShiftId>,
        settled: &BillingBillSettled,
    ) -> Result<(), DomainError> {
        let Some(record) = shift_id.and_then(|shift_id| self.shifts.get_mut(&shift_id)) else {
            return Ok(());
        };
        for component in settled.tax_lines.iter().flat_map(|line| &line.components) {
            billing::add_component(
                &mut record.tax_components,
                TaxComponentLine {
                    name: component.name.clone(),
                    rate_basis_points: component.rate_basis_points,
                    tax: component.tax,
                },
            )?;
        }
        Ok(())
    }

    /// Adds cash taken on a bill to `shift_id`'s rollup, if a shift took it.
    fn collect_cash(
        &mut self,
        shift_id: Option<ShiftId>,
        amount: Money,
    ) -> Result<(), DomainError> {
        if let Some(shift_id) = shift_id
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
    /// Whether a replacement machine has taken this store
    /// ([ADR-0123](../../../docs/adr/0123-a-superseded-box-opens-nothing-new.md)).
    ///
    /// Owned here and lent as one `Arc`, so the config-pull loop that decides it, the four commands
    /// that refuse on it and the response header that reports it are all reading the same value.
    /// `Active` until a pull says otherwise, which is every box that has never been issued a lease.
    standing: Arc<CurrentStanding>,
    /// What the status bar says about the cloud link and the outbox
    /// ([ADR-0137](../../../docs/adr/0137-a-deep-outbox-warns-and-never-refuses.md)). Written by the
    /// outbox drain and the heartbeat, read by `GET /api/sync`.
    sync: SyncStatus,
    /// What the SNTP probe last measured of this box's own clock (roadmap-v3 PF4,
    /// [`crate::sntp`]). Written by the probe loop `main` starts, read by `GET /api/sync`; a
    /// composition that starts no loop — a test, the on-fakes example — reports it unmeasured.
    clock_status: ClockStatus,
    /// The guest notes of the lines still open, held in memory and nowhere else
    /// ([ADR-0157](../../../docs/adr/0157-a-guest-note-lives-in-the-stores-memory-for-the-service.md)).
    ///
    /// Beside the projection rather than inside it, because the projection is rebuilt from the log
    /// and a note is precisely what the log does not hold. Locked on its own and never while the
    /// projection's lock is held.
    notes: Mutex<LineNotes>,
    /// Consecutive wrong PINs per person
    /// ([ADR-0030](../../../docs/adr/0030-pairing-and-offline-auth.md)), shared by the sign-in route
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
/// transaction ([ADR-0064](../../../docs/adr/0064-edge-order-in.md)): the caller's key and the two
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

impl<S: EventStore> Edge<S> {
    /// The instant before which retention may delete a synced event
    /// ([ADR-0145](../../../docs/adr/0145-the-edge-keeps-events-until-synced-and-n-days-old.md)):
    /// the store's [`EdgeSession::event_log_days`] ago, or a day before the oldest business still
    /// open, whichever is earlier.
    ///
    /// The day's margin is for a clock that stepped: an event stamped a little before the order it
    /// belongs to opened must still survive. The store adds the two rules only it can apply (keep
    /// what is unsynced, keep the chain head); this is everything the projection knows.
    ///
    /// # Panics
    ///
    /// If the projection lock is poisoned — unreachable, as its critical section only reads.
    #[must_use]
    pub fn retention_horizon(&self, now: Timestamp) -> Timestamp {
        const DAY_MS: i64 = 86_400_000;
        let session = self.session();
        let now_ms = now.as_milliseconds_since_epoch();
        let aged = now_ms.saturating_sub(i64::from(session.event_log_days).saturating_mul(DAY_MS));
        let open = self
            .lock_projection()
            .oldest_open_work(&session)
            .map_or(i64::MAX, |began| {
                i64::try_from(began)
                    .unwrap_or(i64::MAX)
                    .saturating_sub(DAY_MS)
            });
        Timestamp::from_milliseconds_since_epoch(aged.min(open).max(0)).unwrap_or(Timestamp::EPOCH)
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
    /// The two staff-decision arms of [`Self::fold`], lifted out to keep that function inside the
    /// line budget rather than by dropping the comments that explain it.
    ///
    /// Both payloads carry an `order_id` and nothing else the projection needs — the rejection's
    /// `reason_code_id` belongs to the audit trail, not to the hold, which only has to know that a
    /// decision was made and which way (ADR-0116).
    fn fold_staff_decision(
        projection: &mut Projection,
        envelope: &EventEnvelope<RawPayload>,
        known: EventType,
    ) -> Result<(), AppError> {
        let (order_id, decision) = if known == EventType::SalesOrderConfirmedByStaff {
            let event: SalesOrderConfirmedByStaff =
                envelope.data.decode().map_err(AppError::Encode)?;
            (event.order_id, StaffDecision::Confirmed)
        } else {
            let event: SalesOrderRejectedByStaff =
                envelope.data.decode().map_err(AppError::Encode)?;
            (event.order_id, StaffDecision::Rejected)
        };
        projection.record_staff_decision(order_id, decision);
        Ok(())
    }

    /// Folds a bill's life back onto the projection: opened, paid into, settled.
    ///
    /// Extracted from [`fold`](Self::fold) so that match stays inside its line budget — it had
    /// reached exactly the ceiling, which is why adding the two void arms needed room made rather
    /// than found. The three arms belong together anyway: each one is the same bill moving on, and
    /// two of them also move the table it sits at.
    fn fold_billing(
        projection: &mut Projection,
        envelope: &EventEnvelope<RawPayload>,
        known: EventType,
    ) -> Result<(), AppError> {
        match known {
            EventType::BillingDiscountApplied => {
                let event: BillingDiscountApplied =
                    envelope.data.decode().map_err(AppError::Encode)?;
                // Rebuilt from the log rather than from a stored total, so a store that restarts
                // mid-service quotes the guest the reduced figure it already quoted them. Without
                // this arm a discount survived the commit and vanished on the next boot — and the
                // bill would settle at full price with the guest holding a receipt that said
                // otherwise.
                projection
                    .add_reduction(event.bill_id, event.kind.known(), event.amount)
                    .map_err(AppError::Domain)?;
            }
            EventType::BillingFeeWaived => {
                let event: BillingFeeWaived = envelope.data.decode().map_err(AppError::Encode)?;
                // So a restart charges what the till showed: the fee stays off the bill.
                projection.waive_fee_record(event.bill_id, event.fee_id);
            }
            EventType::BillingBillOpened => {
                let event: BillingBillOpened = envelope.data.decode().map_err(AppError::Encode)?;
                // The bill is recorded whether or not a table is found. A bill event names its
                // order, never a table (ADR-0093), and a counter order has none — while this arm
                // was inside `if let Some(table_id)`, a takeaway bill was dropped on every rebuild,
                // so a restart between opening and settling one lost it entirely and the guest
                // could not be charged.
                let table_id = projection.table_for_order(event.order_id);
                projection.open_bill_record(
                    event.bill_id,
                    BillRecord {
                        order_id: event.order_id,
                        table_id,
                        state: BillState::Open,
                        // Empty on a bill opened before the field existed, which `covered_lines`
                        // reads as "every unvoided line on the order" — what such a bill meant.
                        order_line_ids: event.order_line_ids,
                        discount: None,
                        comp: None,
                        // Empty on a bill opened before the field existed: it charges no fee, which
                        // is what it charged when it was opened.
                        fee_wholes: billing::fee_wholes(&event.fee_rules),
                        fee_rules: event.fee_rules,
                        // A waive is its own event, folded after this one.
                        waived_fee_ids: Vec::new(),
                    },
                );
                if let Some(table_id) = table_id {
                    projection.set_table(table_id, TableState::AwaitingPayment);
                }
            }
            EventType::BillingPaymentCaptured => {
                let event: BillingPaymentCaptured =
                    envelope.data.decode().map_err(AppError::Encode)?;
                // Only cash reaches the drawer, and the roll-up is the reason payments are evented. The
                // payment names its drawer's shift on its envelope (ADR-0167 decision 5); one
                // written without one went to the store's drawer's.
                if event.method.known() == PaymentMethod::Cash {
                    let shift = envelope.shift_id.or(projection.open_shift_of(None));
                    projection.collect_cash(shift, event.applied_to_bill)?;
                }
            }
            EventType::BillingBillSettled => {
                let event: BillingBillSettled = envelope.data.decode().map_err(AppError::Encode)?;
                projection.fold_settle(
                    &event,
                    envelope.shift_id,
                    envelope.business_date,
                    envelope.event_time,
                )?;
            }
            EventType::BillingReceiptReprinted => {
                let event: BillingReceiptReprinted =
                    envelope.data.decode().map_err(AppError::Encode)?;
                projection.record_copy(event.bill_id, event.copy_number);
            }
            // Unreachable: `fold` delegates only the arms above.
            _ => {}
        }
        Ok(())
    }

    /// Folds a split or a merge back onto the projection
    /// ([ADR-0128](../../../docs/adr/0128-a-bill-splits-and-merges.md)), with what each does to the
    /// bills' fees (ADR-0159 decision 6). Extracted from [`fold`](Self::fold) to keep it inside its
    /// line budget, as [`Self::fold_void`] was.
    fn fold_regroup(
        projection: &mut Projection,
        envelope: &EventEnvelope<RawPayload>,
        known: EventType,
    ) -> Result<(), AppError> {
        if known == EventType::BillingBillSplit {
            let event: BillingBillSplit = envelope.data.decode().map_err(AppError::Encode)?;
            // Each part's own `billing.bill.opened`, in the same transaction, folded its lines and
            // its share of each fee; it takes its source's wholes and waived fees here.
            projection.inherit_fees(event.source_bill_id, &event.resulting_bill_ids);
            projection.set_bill_state(event.source_bill_id, BillState::Split);
            return Ok(());
        }
        let event: BillingBillMerged = envelope.data.decode().map_err(AppError::Encode)?;
        // The target takes their lines, rebuilt from what each bill already records rather than from
        // a field on this event: a bill covers lines from the moment it exists, and carrying the
        // union here too would make the same fact true in two places.
        let mut lines = projection
            .bill(event.target_bill_id)
            .map(|target| target.order_line_ids)
            .unwrap_or_default();
        // The fees from what each bill holds, in the event's order, as the merge decided them.
        if let Some(fees) = projection.merged_fees(event.target_bill_id, &event.merged_bill_ids)? {
            projection.set_bill_fees(event.target_bill_id, fees);
        }
        for absorbed_id in event.merged_bill_ids {
            if let Some(absorbed) = projection.bill(absorbed_id) {
                lines.extend(absorbed.order_line_ids);
            }
            projection.set_bill_state(absorbed_id, BillState::Merged);
        }
        lines.sort_unstable();
        lines.dedup();
        projection.set_bill_lines(event.target_bill_id, lines);
        Ok(())
    }

    /// Folds a void back onto the projection — a line's or a bill's.
    ///
    /// Extracted from [`fold`](Self::fold) to keep it inside its line budget, the same way
    /// `fold_staff_decision` was. Both events had **no arm at all** before roadmap B2.2, so a box
    /// that restarted after a void came back believing the line was still fired and the bill still
    /// open: it would have re-fired the line to the kitchen and charged for a bill somebody
    /// cancelled.
    fn fold_void(
        projection: &mut Projection,
        envelope: &EventEnvelope<RawPayload>,
        known: EventType,
    ) -> Result<(), AppError> {
        if known == EventType::SalesOrderLineVoided {
            let event: SalesOrderLineVoided = envelope.data.decode().map_err(AppError::Encode)?;
            projection.set_line_state(event.order_line_id, OrderLineState::Voided);
        } else {
            let event: BillingBillVoided = envelope.data.decode().map_err(AppError::Encode)?;
            projection.void_bill_record(event.bill_id);
        }
        Ok(())
    }

    /// The `sales.order.opened` arm of [`Self::fold`], lifted out to keep that function inside its
    /// line budget.
    fn fold_order_opened(
        projection: &mut Projection,
        envelope: &EventEnvelope<RawPayload>,
    ) -> Result<(), AppError> {
        let event: SalesOrderOpened = envelope.data.decode().map_err(AppError::Encode)?;
        // An unrecognised channel from a newer sender is recorded as "no answer" rather
        // than failing the whole replay: a box that cannot rebuild does not trade, and a
        // guessed variant would tax a bill at a rate nobody chose.
        projection.open_order_origin(event.order_id, event.channel.require().ok(), event.table_id);
        // A waiter's order already sat down on its `sales.table.opened`; a guest's has no
        // other record of its table (`seat_guest_order`). Seating either twice is a no-op.
        if let Some(table_id) = event.table_id {
            projection.seat_guest_order(table_id, event.order_id);
        }
        Ok(())
    }

    /// Replays an order that ended owing nothing: a table released with nothing sold on it
    /// ([`Edge::release_table`]), or a guest order staff refused.
    fn fold_order_closed(
        projection: &mut Projection,
        envelope: &EventEnvelope<RawPayload>,
    ) -> Result<(), AppError> {
        let event: SalesOrderClosed = envelope.data.decode().map_err(AppError::Encode)?;
        projection.close_order(event.order_id);
        Ok(())
    }

    /// Replays guests moving to another table: the table they left waits to be cleared, and the one
    /// they moved to is seated with their order ([`Edge::transfer_table`]).
    fn fold_transfer(
        projection: &mut Projection,
        envelope: &EventEnvelope<RawPayload>,
    ) -> Result<(), AppError> {
        let event: SalesTableTransferred = envelope.data.decode().map_err(AppError::Encode)?;
        projection.set_table(event.from_table_id, TableState::NeedsCleaning);
        projection.set_table(event.to_table_id, TableState::Occupied);
        projection.move_order(event.order_id, event.from_table_id, event.to_table_id);
        Ok(())
    }

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
            EventType::SalesOrderOpened => Self::fold_order_opened(projection, envelope)?,
            EventType::SalesOrderConfirmedByStaff | EventType::SalesOrderRejectedByStaff => {
                Self::fold_staff_decision(projection, envelope, known)?;
            }
            EventType::SalesTableClosed => {
                let event: SalesTableClosed = envelope.data.decode().map_err(AppError::Encode)?;
                projection.set_table(event.table_id, TableState::Free);
            }
            EventType::SalesOrderClosed => Self::fold_order_closed(projection, envelope)?,
            EventType::SalesTableTransferred => Self::fold_transfer(projection, envelope)?,
            EventType::SalesTableMerged => {
                let event: SalesTableMerged = envelope.data.decode().map_err(AppError::Encode)?;
                projection.merge_order(event.target_order_id, event.merged_order_id);
            }
            EventType::SalesOrderLineAdded => {
                let event: SalesOrderLineAdded =
                    envelope.data.decode().map_err(AppError::Encode)?;
                projection.add_line(event.order_line_id, LineRecord::from(event));
            }
            EventType::SalesOrderLineFired => {
                let event: SalesOrderLineFired =
                    envelope.data.decode().map_err(AppError::Encode)?;
                projection.fold_fired(event.order_line_id, event.fire_time, event.station_id);
            }
            EventType::SalesOrderLineUpdated => {
                let event: SalesOrderLineUpdated =
                    envelope.data.decode().map_err(AppError::Encode)?;
                projection.set_line_quantity(event.order_line_id, event.quantity, event.line_total);
            }
            EventType::SalesOrderLineVoided | EventType::BillingBillVoided => {
                Self::fold_void(projection, envelope, known)?;
            }
            EventType::BillingBillOpened
            | EventType::BillingPaymentCaptured
            | EventType::BillingBillSettled
            | EventType::BillingReceiptReprinted
            | EventType::BillingDiscountApplied
            | EventType::BillingFeeWaived => {
                Self::fold_billing(projection, envelope, known)?;
            }
            // A split's source and a merge's absorbed bills are terminal, and a store that replayed
            // its log without them would reopen a bill somebody has already been charged for
            // through its parts (ADR-0128 decision 10). Each part's own `billing.bill.opened`, in
            // the same transaction, folds the lines it covers; the split adds its fees' wholes.
            EventType::BillingBillSplit | EventType::BillingBillMerged => {
                Self::fold_regroup(projection, envelope, known)?;
            }
            EventType::CashShiftOpened => {
                let event: CashShiftOpened = envelope.data.decode().map_err(AppError::Encode)?;
                // A shift that names no till is the store's one drawer's, as every shift was
                // before a till had its own (ADR-0167 decision 5).
                projection.open_shift_record(
                    event.opened_shift_id,
                    ShiftRecord::opened(
                        event.opening_float,
                        event.opening_float.currency_code,
                        (event.terminal_device_id, event.assigned_employee_id),
                        envelope.event_time,
                        envelope.business_date,
                    ),
                );
            }
            EventType::CashDrawerPaidIn | EventType::CashDrawerPaidOut => {
                Self::fold_movement(projection, envelope, known)?;
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
            EventType::InventoryItemSoldOut | EventType::InventoryItemRestored => {
                Self::fold_sold_out(projection, envelope, known)?;
            }
            _ => {}
        }
        Ok(())
    }

    /// A paid in or a paid out, on the shift its envelope names (ADR-0165 decision 2), so a box that
    /// restarts mid-shift expects the drawer a running one does. The envelope is the authority on
    /// the shift: a movement is only ever recorded while one is open, and the fallback to the shift
    /// open now is for a record written without one, which nothing writes.
    fn fold_movement(
        projection: &mut Projection,
        envelope: &EventEnvelope<RawPayload>,
        known: EventType,
    ) -> Result<(), AppError> {
        let (movement, amount) = if known == EventType::CashDrawerPaidIn {
            let event: CashDrawerPaidIn = envelope.data.decode().map_err(AppError::Encode)?;
            (CashMovement::PaidIn, event.amount)
        } else {
            let event: CashDrawerPaidOut = envelope.data.decode().map_err(AppError::Encode)?;
            (CashMovement::PaidOut, event.amount)
        };
        if let Some(shift_id) = envelope.shift_id.or(projection.open_shift_of(None)) {
            projection.record_movement(shift_id, movement, amount)?;
        }
        Ok(())
    }

    /// An item staff marked sold out, or brought back: the in-store 86 a restart must keep.
    fn fold_sold_out(
        projection: &mut Projection,
        envelope: &EventEnvelope<RawPayload>,
        known: EventType,
    ) -> Result<(), AppError> {
        if known == EventType::InventoryItemSoldOut {
            let event: InventoryItemSoldOut = envelope.data.decode().map_err(AppError::Encode)?;
            projection.set_sold_out(event.menu_item_id, true);
        } else {
            let event: InventoryItemRestored = envelope.data.decode().map_err(AppError::Encode)?;
            projection.set_sold_out(event.menu_item_id, false);
        }
        Ok(())
    }

    /// Everything a bill and its receipt are made of, from **one** read of the order.
    ///
    /// The channel is here for roadmap **B1.2**'s reason: it comes from the order's own
    /// `sales.order.opened`, falling back to the session's only when the order has no answer (a
    /// pre-B1.2 order, or a token this build does not understand). Two callers resolving that
    /// separately is two chances to reach for `session.sales_channel` and reintroduce the defect,
    /// which is exactly how it was written before.
    ///
    /// The **lines** are here for [ADR-0129](../../../docs/adr/0129-a-receipt-itemises-what-was-sold.md)'s:
    /// the receipt's rows have to sum to the subtotal printed under them, and they do so only if they
    /// are the very records the bases were folded from. A second read of the projection would be a
    /// second answer — a line added between the two reads, and a document that does not add up.
    ///
    /// # Errors
    ///
    /// [`AppError`] if a line's amounts cannot be summed.
    fn order_snapshot(
        &self,
        order_id: OrderId,
        session: &EdgeSession,
    ) -> Result<OrderSnapshot, AppError> {
        self.snapshot_of(order_id, None, session)
    }

    /// The same read, narrowed to the lines **one bill** covers
    /// ([ADR-0128](../../../docs/adr/0128-a-bill-splits-and-merges.md) decision 1).
    ///
    /// This is the difference a split turns on. Before it, what a bill owed was assembled from every
    /// line of its order — so two bills on one order would each have owed all of it, and there was
    /// nothing to partition. A bill now answers for its own lines and nobody else's.
    ///
    /// # Errors
    ///
    /// [`AppError`] if a line's amounts cannot be summed.
    fn bill_snapshot(
        &self,
        bill: &BillRecord,
        session: &EdgeSession,
    ) -> Result<OrderSnapshot, AppError> {
        self.snapshot_of(bill.order_id, Some(&bill.order_line_ids), session)
    }

    /// One read of an order, optionally narrowed to a set of its lines.
    ///
    /// `covers` of `None` — or an empty set, which is what a bill opened before ADR-0128's field
    /// existed carries — means every line of the order, which is what such a bill has always meant.
    fn snapshot_of(
        &self,
        order_id: OrderId,
        covers: Option<&[OrderLineId]>,
        session: &EdgeSession,
    ) -> Result<OrderSnapshot, AppError> {
        // The guard is taken and dropped inside the block: `class_bases` does arithmetic that can
        // fail, and holding the projection lock across a `?` is how a poisoned lock happens.
        let (lines, channel) = {
            let projection = self.lock_projection();
            let lines = match covers {
                Some(covered) if !covered.is_empty() => projection
                    .identified_lines_for_order(order_id)
                    .into_iter()
                    .filter(|(id, _)| covered.contains(id))
                    .map(|(_, record)| record)
                    .collect(),
                _ => projection.lines_for_order(order_id),
            };
            (lines, projection.order_channel(order_id))
        };
        let class_bases = Self::class_bases(&lines)?;
        let bill_lines = Self::bill_lines(&lines);
        Ok(OrderSnapshot {
            lines,
            class_bases,
            bill_lines,
            sales_channel: channel.unwrap_or(session.sales_channel),
        })
    }

    /// The lines that were actually sold: everything but the voided ones.
    ///
    /// One definition, two readers — [`Self::class_bases`] taxes them and [`Self::receipt_lines`]
    /// prints them. Written as a shared filter rather than the same `continue` in two loops, because
    /// the two disagreeing is precisely a receipt whose rows do not sum to its own subtotal
    /// (ADR-0129 decision 4).
    fn sold_lines(lines: &[LineRecord]) -> impl Iterator<Item = &LineRecord> {
        lines
            .iter()
            .filter(|line| line.state != OrderLineState::Voided)
    }

    /// The rows a receipt prints, in the order the bases were folded in (ADR-0129 decision 1).
    ///
    /// Straight off the record, which is straight off the event: the name and the unit price are the
    /// ones the guest agreed to at add time, not the ones the menu carries now (§14.2).
    fn receipt_lines(lines: &[LineRecord]) -> Vec<ReceiptLine> {
        Self::sold_lines(lines)
            .map(|line| ReceiptLine {
                menu_item_id: line.menu_item_id,
                display_name: line.display_name.clone(),
                quantity: line.quantity,
                unit_price: line.unit_price,
                line_total: line.line_total,
                modifier_display_names: line.modifier_display_names.clone(),
                modifier_menu_item_ids: line.modifier_menu_item_ids.clone(),
            })
            .collect()
    }

    /// The sold lines as a fee rule matches them: its item, quantity, net and class
    /// ([ADR-0159](../../../docs/adr/0159-a-fee-is-configuration.md) decision 2). The same lines
    /// [`Self::class_bases`] sums, through the same filter, so each class's lines come to its base
    /// and [`billing::assemble`] never finds them another bill's.
    fn bill_lines(lines: &[LineRecord]) -> Vec<BillLine> {
        Self::sold_lines(lines).map(Self::bill_line).collect()
    }

    /// One sold line as a fee rule matches it. Its net is the extended total captured at add time,
    /// the figure [`Self::class_bases`] sums.
    fn bill_line(line: &LineRecord) -> BillLine {
        BillLine {
            menu_item_id: line.menu_item_id,
            quantity: line.quantity,
            net: line.line_total,
            tax_class_id: line.tax_class_id,
        }
    }

    /// Groups an order's lines into a pre-tax base per tax class, the input [`billing::assemble`]
    /// takes. A voided line is owed nothing, so it contributes nothing.
    fn class_bases(lines: &[LineRecord]) -> Result<Vec<ClassBase>, AppError> {
        let mut bases: Vec<ClassBase> = Vec::new();
        for line in Self::sold_lines(lines) {
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

    /// Builds the bill-assembly input from the session's tax configuration, one read of the lines,
    /// and the fee rules the bill is computed from. The P5 bootstrap runs with no bill-level
    /// discount, no service charge and no cash rounding; the cloud config tree (P7) supplies those,
    /// and the shape here is ready for them.
    ///
    /// The class bases and the lines the rules match come from the same [`OrderSnapshot`], so
    /// they always describe one bill: [`DomainError::LinesDoNotMatchBases`] cannot follow from a
    /// read, and answers as a server error if it ever does.
    fn bill_input<'a>(
        session: &'a EdgeSession,
        order: &'a OrderSnapshot,
        fee_rules: &'a [FrozenFee],
        waived_fee_ids: &'a [FeeId],
        reductions: (Money, Money),
    ) -> BillInput<'a> {
        let currency = session.currency;
        let (bill_discount, comps) = reductions;
        BillInput {
            currency_code: currency,
            class_bases: &order.class_bases,
            bill_discount,
            comps,
            service_charge: Money::zero(currency),
            service_charge_taxable: true,
            service_charge_tax_class: None,
            rates: &session.tax_rates,
            // The order's channel, from the snapshot — not `session.sales_channel`, which is a
            // store-wide constant and therefore the wrong rate for every order that did not come in
            // on it.
            sales_channel: order.sales_channel,
            cash_rounding_increment: session.cash_rounding_increment,
            tax_rounding: session.tax_rounding.rounding(),
            prices_include_tax: session.prices_include_tax,
            fee_rules,
            waived_fee_ids,
            lines: &order.bill_lines,
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
        self.prepare_on(ctx, payload, self.current_shift_id())
    }

    /// [`Self::prepare`], stamped with `shift` rather than the store's drawer's shift: what an act
    /// on one drawer's shift is stamped with (ADR-0167 decision 5).
    fn prepare_on<P: EventPayload + serde::Serialize>(
        &self,
        ctx: &DecisionCtx,
        payload: &P,
        shift: Option<ShiftId>,
    ) -> Result<(EventEnvelope<RawPayload>, ServerMessage), AppError> {
        let published = serde_json::to_value(payload).unwrap_or(serde_json::Value::Null);
        let event_type = EventTypeRef::from_known(P::EVENT_TYPE).as_str().to_owned();
        let envelope = self.envelope(ctx, payload, shift)?;
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
            // Unstamped here. The chain is assigned at append, inside the transaction,
            // because that is the only place the previous head is known (ADR-0131).
            chain: None,
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

    /// Locks the held guest notes, recovering from a poisoned lock as [`Self::lock_projection`] does.
    ///
    /// Never taken while the projection's lock is held, nor the projection's while this is, so the
    /// two cannot deadlock.
    fn lock_notes(&self) -> std::sync::MutexGuard<'_, LineNotes> {
        self.notes
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// The guest note the edge holds for a line, for its kitchen ticket (ADR-0157).
    #[must_use]
    pub fn line_note(&self, order_line_id: OrderLineId) -> Option<NoteText> {
        self.lock_notes().get(order_line_id).cloned()
    }

    /// How many guest notes the edge holds — the figure a test reads to see one forgotten.
    #[must_use]
    pub fn held_note_count(&self) -> usize {
        self.lock_notes().len()
    }

    /// Forgets every note no screen can show any more: a voided line's, and every line's on an
    /// order that has left the open orders (ADR-0157 decision 2), except a line the kitchen board
    /// still shows for a paid order without a table, until it is bumped or its day ends (ADR-0161).
    ///
    /// A line the projection has not folded yet is kept, because its note is held a moment before
    /// its write commits.
    fn forget_notes_off_screen(&self) {
        let held = self.lock_notes().held();
        if held.is_empty() {
            return;
        }
        let gone: Vec<OrderLineId> = {
            let projection = self.lock_projection();
            // Each line's order as the projection holds it now, rather than the one its note was
            // held under: a guest's line that joined the table's order is on that order now.
            held.into_iter()
                .filter(|(order_line_id, _held_under)| {
                    projection.line(*order_line_id).is_some_and(|record| {
                        record.state == OrderLineState::Voided
                            || (!projection.live.contains(&record.order_id)
                                && !projection.still_cooking(*order_line_id, &record))
                    })
                })
                .map(|(order_line_id, _)| order_line_id)
                .collect()
        };
        let mut notes = self.lock_notes();
        for order_line_id in gone {
            notes.forget(order_line_id);
        }
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
        shift: Option<ShiftId>,
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
            shift_id: shift,
            // Unstamped here. The chain is assigned at append, inside the transaction,
            // because that is the only place the previous head is known (ADR-0131).
            chain: None,
            data,
        })
    }

    /// A fresh id for a print job no event stands behind — a manager's test page. Other jobs take
    /// the id of the event that caused them, which is what makes a retried print idempotent.
    #[must_use]
    pub fn print_job_id(&self) -> pos_proto::ids::EventId {
        pos_proto::ids::EventId::new(self.next_ulid())
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
    use std::collections::BTreeMap;
    use std::sync::Arc;

    use super::{Edge, EdgeSession, LineDraft, StoreIdentity};
    use crate::receipt::InMemoryReceipts;
    use pos_core::billing::Payment;
    use pos_core::decision::Actor;
    use pos_fakes::FakeStore;
    use pos_proto::ids::{DeviceId, EmployeeId, MenuItemId, OrderId, StoreId, TableId};
    use pos_proto::menu::MenuCatalog;
    use pos_proto::money::{CurrencyCode, Money, Ratio};
    use pos_proto::quantity::Quantity;
    use pos_proto::text::DisplayName;
    use pos_proto::ulid::Ulid;
    use pos_proto::{BillState, Open, PaymentMethod, SalesChannel};

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

    // ---- the order index (F2) ---------------------------------------------------------------

    /// What the index replaced: every line the store holds, filtered to one order, in id order.
    fn lines_by_scan(projection: &super::Projection, order_id: OrderId) -> Vec<super::OrderLineId> {
        let mut ids: Vec<super::OrderLineId> = projection
            .lines
            .iter()
            .filter(|(_, record)| record.order_id == order_id)
            .map(|(id, _)| *id)
            .collect();
        ids.sort_unstable();
        ids
    }

    /// What the live set replaced: every order with a line and no settled bill, by a full scan.
    fn open_orders_by_scan(projection: &super::Projection) -> Vec<OrderId> {
        let mut orders: Vec<OrderId> = projection
            .lines
            .values()
            .map(|record| record.order_id)
            .filter(|order_id| {
                projection
                    .bill_for_order(*order_id)
                    .and_then(|bill_id| projection.bill(bill_id))
                    .is_none_or(|bill| bill.state != BillState::Settled)
            })
            .collect();
        orders.sort_unstable();
        orders.dedup();
        orders
    }

    /// The index and the live set agree with the scans they replaced.
    fn assert_index_agrees(edge: &Edge<FakeStore>, step: &str) {
        let projection = edge.lock_projection();
        assert_eq!(
            projection.open_orders(),
            open_orders_by_scan(&projection),
            "live orders after {step}"
        );
        let mut every_order: Vec<OrderId> = projection
            .lines
            .values()
            .map(|record| record.order_id)
            .collect();
        every_order.sort_unstable();
        every_order.dedup();
        for order_id in every_order {
            let indexed: Vec<super::OrderLineId> = projection
                .identified_lines_for_order(order_id)
                .into_iter()
                .map(|(id, _)| id)
                .collect();
            assert_eq!(
                indexed,
                lines_by_scan(&projection, order_id),
                "lines after {step}"
            );
        }
    }

    #[test]
    fn the_order_index_agrees_with_a_scan_through_a_whole_service() {
        pos_fakes::executor::run_ready(async {
            let edge = edge();
            let (a, b) = (
                TableId::new(Ulid::from_u128(401)),
                TableId::new(Ulid::from_u128(402)),
            );

            edge.seat_table(actor(), a, None).await.expect("seats a");
            let mut on_a = Vec::new();
            for _ in 0..4 {
                on_a.push(
                    edge.add_line(actor(), a, a_line())
                        .await
                        .expect("adds")
                        .order_line_id,
                );
            }
            assert_index_agrees(&edge, "four lines on a");
            let order_a = edge
                .lock_projection()
                .order_for_table(a)
                .expect("a has an order");

            edge.seat_table(actor(), b, None).await.expect("seats b");
            edge.add_line(actor(), b, a_line())
                .await
                .expect("adds to b");
            assert_index_agrees(&edge, "a second table");

            let bill = edge.open_bill(actor(), a).await.expect("opens a's bill");
            assert_index_agrees(&edge, "a bill opened");

            let parts = edge
                .split_bill(
                    actor(),
                    bill.bill_id,
                    vec![vec![on_a[0], on_a[1]], vec![on_a[2], on_a[3]]],
                )
                .await
                .expect("splits");
            assert_index_agrees(&edge, "a split");

            edge.merge_bills(actor(), parts[0], vec![parts[1]])
                .await
                .expect("merges back");
            assert_index_agrees(&edge, "a merge");

            edge.settle_bill(actor(), parts[0], vec![cash(660_000)], None)
                .await
                .expect("settles a");
            assert_index_agrees(&edge, "a settle");
            // And a count: a split merged back and settled is finished. The single bill pointer
            // this replaced named the absorbed part, so the paid order stayed live.
            assert!(
                !edge.lock_projection().open_orders().contains(&order_a),
                "a's order is paid"
            );

            edge.clean_table(actor(), a).await.expect("cleans a");
            edge.seat_table(actor(), a, None).await.expect("re-seats a");
            edge.add_line(actor(), a, a_line())
                .await
                .expect("a new order on a");
            assert_index_agrees(&edge, "a re-seat");
        });
    }

    #[test]
    fn a_settled_history_does_not_grow_the_live_set() {
        pos_fakes::executor::run_ready(async {
            let edge = edge();
            let table = TableId::new(Ulid::from_u128(410));
            for _ in 0..50 {
                edge.seat_table(actor(), table, None).await.expect("seats");
                edge.add_line(actor(), table, a_line()).await.expect("adds");
                let bill = edge.open_bill(actor(), table).await.expect("bills");
                edge.settle_bill(actor(), bill.bill_id, vec![cash(165_000)], None)
                    .await
                    .expect("settles");
                edge.clean_table(actor(), table).await.expect("cleans");
            }
            edge.seat_table(actor(), table, None)
                .await
                .expect("seats the live one");
            edge.add_line(actor(), table, a_line()).await.expect("adds");

            let projection = edge.lock_projection();
            assert_eq!(
                projection.live.len(),
                1,
                "fifty settled orders and one live one"
            );
            assert_eq!(
                projection.order_lines.len(),
                51,
                "every order is still indexed"
            );
        });
    }

    #[test]
    fn a_projection_rebuilt_twice_sells_each_line_once() {
        pos_fakes::executor::run_ready(async {
            let store = FakeStore::default();
            let live = Edge::new(
                store.clone(),
                identity(),
                EdgeSession::bootstrap(),
                Arc::new(InMemoryReceipts::new()),
            )
            .expect("seeds");
            let table = TableId::new(Ulid::from_u128(420));
            live.seat_table(actor(), table, None).await.expect("seats");
            for _ in 0..3 {
                live.add_line(actor(), table, a_line()).await.expect("adds");
            }

            let restarted = Edge::new(
                store,
                identity(),
                EdgeSession::bootstrap(),
                Arc::new(InMemoryReceipts::new()),
            )
            .expect("seeds");
            restarted.rebuild().await.expect("rebuilds");
            restarted.rebuild().await.expect("rebuilds again");

            assert_eq!(restarted.live_orders(), live.live_orders());
            assert_eq!(
                restarted
                    .live_orders()
                    .first()
                    .map(|order| order.lines.len()),
                Some(3),
                "no line is indexed twice"
            );
        });
    }
}
