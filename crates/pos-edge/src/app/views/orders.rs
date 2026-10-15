// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! What the edge answers about tables, orders and their lines, and what a fire tells the kitchen.

use pos_proto::ids::{
    BillId, CourseId, MenuItemId, OrderId, OrderLineId, StationId, TableId, TaxClassId,
};
use pos_proto::money::{Money, Ratio};
use pos_proto::quantity::Quantity;
use pos_proto::text::DisplayName;
use pos_proto::time::{BusinessDate, Timestamp};
use pos_proto::{OrderLineState, SalesChannel, TableState};

use crate::line_notes::NoteText;

/// One line of a guest order waiting for staff confirmation (ADR-0116).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WaitingLine {
    /// The name the guest saw when they ordered.
    pub display_name: String,
    /// How many.
    pub quantity: Quantity,
    /// The extended total, as captured.
    pub line_total: Money,
}

/// What a table looks like to a caller after a command.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TableView {
    /// The table.
    pub table_id: TableId,
    /// Its state after the command.
    pub state: TableState,
}

/// What moving guests to another table did: the order that went with them, and both tables after.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TransferView {
    /// The order that moved.
    pub order_id: OrderId,
    /// The table the guests left, now waiting to be cleared.
    pub from: TableView,
    /// The table they moved to, now seated with their order.
    pub to: TableView,
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
    /// The modifiers chosen for this line. Their prices are already inside `unit_price`; these ids
    /// are what the kitchen prints and what a fire consumes (§8).
    pub modifier_menu_item_ids: Vec<MenuItemId>,
    /// Whether a guest note was written — its text never enters the log.
    pub note_present: bool,
}

/// What a till sends to add a line to an order by its id (ADR-0146): the guest's choice, and no
/// price. The edge prices it from its own price book at the order's channel.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OrderLineChoice {
    /// The item.
    pub menu_item_id: MenuItemId,
    /// How many.
    pub quantity: Quantity,
    /// The chosen modifiers, each a price-book item.
    pub modifier_menu_item_ids: Vec<MenuItemId>,
    /// Seat, when seats are enabled.
    pub seat: Option<u16>,
    /// Course, when courses are enabled.
    pub course_id: Option<CourseId>,
    /// Whether the line carries a free-text note (the note itself never enters an event).
    pub note_present: bool,
}

/// A counter order the till has just opened (ADR-0146).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CounterOrderOpened {
    /// The new order.
    pub order_id: OrderId,
    /// The trading day it opened on, which the queue number is counted within.
    pub business_date: BusinessDate,
}

/// What a line looks like to a caller after a command.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LineView {
    /// The order the line belongs to.
    pub order_id: OrderId,
    /// The line.
    pub order_line_id: OrderLineId,
    /// Its state after the command.
    pub state: OrderLineState,
    /// Present only on a fire: what the kitchen was told to make, for the caller to print
    /// ([ADR-0100](../../../../../docs/adr/0100-receipt-and-ticket-printing.md)).
    pub fired: Option<FiredLine>,
}

/// What a fire tells the kitchen, for the caller to turn into a ticket.
///
/// The ids, not the names: resolving an item's name is the *session's* job (its published menu
/// carries the store's spelling and its locale), and the application loop deliberately does not hold
/// presentation. The edge still holds no printer — this is the caller's material, exactly as
/// [`BillView::print_receipt`](crate::app::BillView::print_receipt) is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FiredLine {
    /// The station the published routing sent this to (ADR-0072).
    pub station_id: StationId,
    /// The item being made.
    pub menu_item_id: MenuItemId,
    /// How many — a [`Quantity`], because a split item is half of one.
    pub quantity: Quantity,
    /// The modifiers chosen, in the order they were added.
    pub modifier_menu_item_ids: Vec<MenuItemId>,
    /// Whether a guest note was written for the line, so a ticket whose note the edge no longer
    /// holds says one was lost rather than printing nothing (ADR-0157).
    pub note_present: bool,
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

/// One line of a counter order, as the counter list shows it.
///
/// Enough for a cashier to recognise the order a customer is collecting: what it is and how many.
/// The name is resolved from the store's published menu at read time rather than stored on the
/// projection, because `LineRecord` deliberately keeps only what the *event* carried.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CounterOrderLine {
    /// The item's name as the store's published menu spells it.
    pub display_name: DisplayName,
    /// How many.
    pub quantity: Quantity,
}

/// A counter order awaiting payment, as the takeaway screen lists it
/// ([ADR-0093](../../../../../docs/adr/0093-bill-keyed-on-order.md)).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CounterOrderView {
    /// The order to bill.
    pub order_id: OrderId,
    /// The daily number the counter shouted, when the order has one. `None` for an order that was
    /// never given a number — which is why this is read rather than allocated: a read route must
    /// not mint one.
    pub queue_number: Option<u64>,
    /// What is on it, for a cashier to recognise.
    pub items: Vec<CounterOrderLine>,
    /// What the guest owes, assembled by the same `billing::assemble` the settle path runs.
    pub total_due: Money,
    /// The bill already open on this order, if a cashier opened one and the screen has since
    /// reloaded. The screen settles **this** bill rather than opening a second one, which the
    /// domain would refuse ([`AppError::BillAlreadyOpen`](crate::app::AppError::BillAlreadyOpen)).
    pub bill_id: Option<BillId>,
}

/// One line of an open order, as a device rebuilding its screens reads it.
///
/// Everything a screen needs to draw the line **and act on it**: the id a fire, a void or a bump is
/// addressed to, what the guest sees, and where the line has got to. Deliberately the same facts
/// the fan-out carries, because the two have to agree — this read is what a device has instead of
/// the events it was not running to hear.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LiveOrderLine {
    /// The line, so the device can fire, void or bump it.
    pub order_line_id: OrderLineId,
    /// The item's name as the store's published menu spells it, falling back to the id for an item
    /// the store has since removed — the same rule [`CounterOrderLine`] follows, and for the same
    /// reason: an order that can no longer be read is worse than one naming an id.
    pub display_name: DisplayName,
    /// How many.
    pub quantity: Quantity,
    /// The extended total the device captured when the line was added (§14.2), never a fresh
    /// lookup — so a screen that rebuilds shows the figure the guest was quoted.
    pub line_total: Money,
    /// Where the line has got to: added, fired, voided.
    pub state: OrderLineState,
    /// Whether a station has marked it prepared (`kitchen.ticket.bumped`). Orthogonal to `state`,
    /// and the reason a kitchen display coming online does not re-show tickets already made.
    pub bumped: bool,
    /// When the line went to the kitchen, or `None` while it is still on the pad.
    pub fired_time: Option<Timestamp>,
    /// What was chosen for the line (ADR-0127). Ids; the caller names them from its own price book,
    /// because an order screen and a kitchen board want today's spelling.
    ///
    /// Carried for the reason the header gives — *"this read is what a device has instead of the
    /// events it was not running to hear"* — and it was not: a kitchen display switched on
    /// mid-service, which is exactly what this read exists for, rebuilt every ticket with no
    /// modifiers on it, so a cook could not tell a 25cm from a 30cm.
    pub modifier_menu_item_ids: Vec<MenuItemId>,
    /// Whose dish it is, or `None` for the table's — the same omission, one field over.
    ///
    /// A server assigns seats so that the food can be put in front of the right person; a till that
    /// reloaded lost every one of them and showed a table's worth of anonymous lines.
    pub seat: Option<u16>,
    /// The course the line goes out on, or `None` for a line on none (ADR-0130).
    ///
    /// The third field to ride here for the header's reason, and the one that would have been the
    /// third to go missing: a till that reloaded mid-service would show every line uncoursed, so the
    /// course controls would offer to send starters that the screen no longer knew were starters.
    pub course_id: Option<CourseId>,
    /// The station it was fired to, `None` while it is still on the pad — what lets a kitchen
    /// board show one station its own tickets and bump them to the right place.
    pub station_id: Option<StationId>,
    /// Whether a guest note was written for the line — from the log, so it survives a restart.
    pub note_present: bool,
    /// The note's text while the edge still holds it (ADR-0157). `None` with `note_present` set is
    /// a note lost to a restart, which a screen says rather than hides.
    pub note: Option<NoteText>,
}

/// An order still owing money, with its lines — what a device reads to rebuild its screens.
///
/// The till's boot already loads the floor, the price book, the button plan and the money settings;
/// what it never loaded is **what is open right now**, so an order, a check and a kitchen board all
/// came back empty after a reload. One read answers for all of them, which is why it is keyed on
/// the order rather than the table: a counter order sits on no table
/// ([ADR-0093](../../../../../docs/adr/0093-bill-keyed-on-order.md)) and the kitchen works from both.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LiveOrderView {
    /// The order.
    pub order_id: OrderId,
    /// The table it sits on, or `None` for a counter order.
    pub table_id: Option<TableId>,
    /// A bill already open on it, so a device that reloads on the payment screen settles **that**
    /// bill rather than asking for a second one the domain would refuse.
    pub bill_id: Option<BillId>,
    /// Every bill still open on it, oldest first: one for an ordinary table, one per unpaid part once
    /// its bill has been split ([ADR-0128](../../../../../docs/adr/0128-a-bill-splits-and-merges.md)).
    ///
    /// `bill_id` names only the newest of them, so a device that reloaded mid-split learned one part
    /// and none of the others. It paid that one, found the table still awaiting payment, and had no
    /// id to settle the rest with — asking for a bill again is refused while one is open.
    pub open_bill_ids: Vec<BillId>,
    /// The channel it was opened on, or `None` for an order this build never saw open or whose
    /// channel it does not know. The counter shows a walk-in the book of its channel, and whether
    /// the guest eats in or takes away, so a till that reloads on one learns the channel here
    /// ([ADR-0160](../../../../../docs/adr/0160-everything-a-store-runs-differently-is-published-configuration.md)
    /// decision 2).
    pub sales_channel: Option<SalesChannel>,
    /// Its lines, in id order.
    pub lines: Vec<LiveOrderLine>,
}
