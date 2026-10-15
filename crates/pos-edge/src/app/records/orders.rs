// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! What the projection remembers about orders and their lines, where an order came from, and where
//! it stands with staff.

use pos_core::billing::{BillLine, ClassBase};
use pos_proto::events::SalesOrderLineAdded;
use pos_proto::ids::{
    BillId, CourseId, MenuItemId, OrderId, OrderLineId, StationId, TableId, TaxClassId,
};
use pos_proto::money::Money;
use pos_proto::quantity::Quantity;
use pos_proto::text::DisplayName;
use pos_proto::time::Timestamp;
use pos_proto::{OrderLineState, SalesChannel};

use crate::fanout::ServerMessage;
use crate::line_notes::NoteText;

/// Puts a line's guest note on the frame that tells the store's devices about it (ADR-0157).
///
/// The frame's payload is a copy of the event made for the fan-out; the event appended to the log is
/// encoded separately and never carries the text.
pub(in crate::app) fn attach_note(message: &mut ServerMessage, note: &NoteText) {
    if let ServerMessage::Event {
        payload: serde_json::Value::Object(fields),
        ..
    } = message
    {
        fields.insert(
            "note".to_owned(),
            serde_json::Value::String(note.as_str().to_owned()),
        );
    }
}

/// One live order as [`Edge::live_orders`] copies it out of the projection, before the lock is
/// released and the view is assembled.
pub(in crate::app) struct LiveSnapshot {
    pub(in crate::app) order_id: OrderId,
    pub(in crate::app) table_id: Option<TableId>,
    pub(in crate::app) bill_id: Option<BillId>,
    pub(in crate::app) open_bill_ids: Vec<BillId>,
    pub(in crate::app) sales_channel: Option<SalesChannel>,
    /// Each line with its id and whether a station has bumped it.
    pub(in crate::app) lines: Vec<(OrderLineId, LineRecord, bool)>,
}

/// What the projection remembers about one order line, so a fire can be decided and its consumption
/// computed, and a bill assembled, without re-reading the event log.
/// `Clone`, not `Copy`: the modifier list is a `Vec`. Cloning a record is a handful of words plus a
/// list that is empty on almost every line, and the alternative — keeping the ids somewhere other
/// than beside the line they belong to — is how they got lost in the first place.
#[derive(Debug, Clone)]
pub(in crate::app) struct LineRecord {
    pub(in crate::app) order_id: OrderId,
    pub(in crate::app) state: OrderLineState,
    pub(in crate::app) menu_item_id: MenuItemId,
    /// The name the guest saw at add time, and the name the receipt prints
    /// ([ADR-0129](../../../docs/adr/0129-a-receipt-itemises-what-was-sold.md)).
    ///
    /// `sales.order_line.added` has always carried it — it is half of §14.2's line snapshot — and
    /// this projection always threw it away, because a fire and a tax-class fold have no use for a
    /// name. Kept now, from the *event* and not from `session.menu`, because a settled bill must not
    /// change when somebody renames an item next week.
    pub(in crate::app) display_name: DisplayName,
    pub(in crate::app) quantity: Quantity,
    /// Whose dish it is, when the store assigns seats, or `None` for the table's.
    ///
    /// `sales.order_line.added` has carried it since seats were added and this projection dropped
    /// it, which is the same omission one field over from the modifiers: a till that reloads had no
    /// way to learn a seat back, so a server rebuilding after a crash saw every line as the table's.
    pub(in crate::app) seat: Option<u16>,
    pub(in crate::app) course_id: Option<CourseId>,
    /// The per-unit price captured at add time, and two things depend on it.
    ///
    /// A **quantity change** re-extends the line from it: `sales.order_line.added` has always
    /// carried it and this projection used to drop it, which left the edge holding an extended total
    /// it could not take apart — re-extending meant dividing by the old quantity to recover a price,
    /// and a rounded one at that. And a **receipt** prints it, as the `đơn giá` Decree 123/2020
    /// Art. 10 asks for ([ADR-0129](../../../docs/adr/0129-a-receipt-itemises-what-was-sold.md)).
    ///
    /// Either way it is the figure the guest was quoted rather than today's menu price — a line
    /// never re-reads the live menu (§14.2), and that holds when it changes as much as when it is
    /// printed. Modifier prices are already inside it.
    pub(in crate::app) unit_price: Money,
    /// The extended total the device captured at add time — the base a bill sums per tax class. A
    /// line never re-reads the live menu (§14.2), so this is authoritative.
    pub(in crate::app) line_total: Money,
    /// The tax class captured at add time, which keys the line into a rate on the bill.
    pub(in crate::app) tax_class_id: TaxClassId,
    /// The modifiers chosen at add time, which a fire consumes alongside the base recipe (§8).
    pub(in crate::app) modifier_menu_item_ids: Vec<MenuItemId>,
    /// The names those modifiers had at add time, which the receipt prints under the line
    /// ([ADR-0144](../../../docs/adr/0144-a-line-records-the-names-of-its-modifiers.md)). Empty for
    /// a line recorded before the event carried them, and its receipt prints no modifiers.
    pub(in crate::app) modifier_display_names: Vec<DisplayName>,
    /// When the line went to the kitchen, or `None` while it is still on the pad.
    ///
    /// Taken from the *envelope* of `sales.order_line.fired` rather than read off a clock here, so
    /// it survives a rebuild: a kitchen screen that reloads mid-service is exactly the screen that
    /// most needs to know a ticket has been waiting eleven minutes, and one that started counting
    /// from its own start-up would report every ticket as new.
    pub(in crate::app) fired_time: Option<Timestamp>,
    /// The station the line was fired to, or `None` while it is still on the pad — from the same
    /// event as [`Self::fired_time`], so a kitchen board that reloads still shows each station only
    /// its own tickets.
    pub(in crate::app) station_id: Option<StationId>,
    /// Whether a guest note was written for the line. From the event, so it survives a restart that
    /// the note's text does not, and a screen can say a note was lost rather than show none
    /// (ADR-0157).
    pub(in crate::app) note_present: bool,
}

impl From<SalesOrderLineAdded> for LineRecord {
    /// The replay's view of an added line.
    ///
    /// `state` is [`OrderLineState::Added`] and not read off the event because the event *is* the
    /// add; a later `sales.order_line.fired` moves it on, and the fold applies those in order.
    fn from(event: SalesOrderLineAdded) -> Self {
        Self {
            order_id: event.order_id,
            state: OrderLineState::Added,
            menu_item_id: event.menu_item_id,
            display_name: event.display_name,
            quantity: event.quantity,
            seat: event.seat,
            course_id: event.course_id,
            unit_price: event.unit_price,
            line_total: event.line_total,
            tax_class_id: event.tax_class_id,
            modifier_menu_item_ids: event.modifier_menu_item_ids,
            modifier_display_names: event.modifier_display_names,
            // An added line has not been fired, so there is no time to record yet. The fold on
            // `sales.order_line.fired` fills it from that event's own envelope.
            fired_time: None,
            station_id: None,
            note_present: event.note_present,
        }
    }
}

/// One read of an order: its lines, the pre-tax base per tax class those lines fold into, and the
/// channel to tax them at. Everything downstream of it — the totals, and the receipt's rows — comes
/// from this one answer, which is what makes the two agree (see [`Edge::order_snapshot`]).
pub(in crate::app) struct OrderSnapshot {
    pub(in crate::app) lines: Vec<LineRecord>,
    pub(in crate::app) class_bases: Vec<ClassBase>,
    /// The sold lines as a fee rule matches them, folded from the same lines as `class_bases`, so
    /// each class's lines always come to its base.
    pub(in crate::app) bill_lines: Vec<BillLine>,
    pub(in crate::app) sales_channel: SalesChannel,
}

/// Where an order came from — the two `sales.order.opened` facts a later decision needs.
///
/// The channel decides the tax rate (roadmap **B1.2**); the channel and the table together decide
/// whether the order is a guest submission awaiting staff confirmation (ADR-0116). Both are
/// `Option` for the same reason: an `UNSPECIFIED` or unrecognised channel token from a newer
/// sender records as `None` rather than a guessed variant, and a tableless order has no table.
///
/// The table is the one the order is served at, so it moves when the guests do
/// (`sales.table.transferred`, [`Projection::move_order`]). A refused guest order hands its table
/// back to the order it displaced by looking for the orders served *there*, and an order that had
/// moved in would otherwise be looked for at the table it left.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::app) struct OrderOrigin {
    pub(in crate::app) channel: Option<SalesChannel>,
    pub(in crate::app) table_id: Option<TableId>,
}

/// What a member of staff decided about a guest's submission (ADR-0116).
///
/// Recorded rather than collapsed to a bool because the two outcomes differ in what follows:
/// a confirmation releases the order to the kitchen, a rejection closes it. A screen that
/// showed "decided" without saying which would be useless to the person who has to explain it
/// to the guest.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::app) enum StaffDecision {
    /// Released to the kitchen.
    Confirmed,
    /// Refused, and the order closed with it.
    Rejected,
}

/// Where an order stands with staff, from the fire path's point of view (ADR-0116).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::app) enum StaffStanding {
    /// A guest submission nobody has decided on yet. The kitchen must not see it.
    Waiting,
    /// A guest submission staff refused. The kitchen must never see it.
    Refused,
    /// Either released by staff, or never a held guest submission at all.
    Fireable,
}
