// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! What the projection records of lines: their state, firing and bumping, the waiting set the
//! kitchen read keeps, the stock they consume, and the open orders and lines it reads back.

use std::collections::{BTreeSet, HashSet};

use pos_core::inventory::StockMovement;
use pos_proto::ids::{MenuItemId, OrderId, OrderLineId, StationId};
// Only the `#[cfg(test)]` stock read names it; the fold itself works in `StockMovement`s.
#[cfg(test)]
use pos_proto::ids::IngredientId;
use pos_proto::money::Money;
use pos_proto::quantity::Quantity;
use pos_proto::time::Timestamp;
use pos_proto::ulid::Ulid;
use pos_proto::{BillState, OrderLineState, TableState};

use crate::app::{EdgeSession, LineRecord, MAX_COOKING, Projection, StaffDecision};

/// The `tracing` target this module logs under: the path it had in `pos_edge`, kept when it moved
/// into this crate so log filters and dashboards see the same target.
const LOG_TARGET: &str = "pos_edge::app::projection::lines";

impl Projection {
    /// Recomputes whether `order_id` is still owing money, from the rule's own inputs.
    pub(in crate::app) fn refresh_live(&mut self, order_id: OrderId) {
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
    pub(in crate::app) fn bound_cooking(&mut self) {
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
    pub(in crate::app) fn still_cooking(
        &self,
        order_line_id: OrderLineId,
        record: &LineRecord,
    ) -> bool {
        record.state == OrderLineState::Fired
            && !self.bumped_lines.contains(&order_line_id)
            && self.cooking.contains_key(&record.order_id)
    }

    /// The ids of an order's lines, in id order, from the index.
    pub(in crate::app) fn line_ids_for_order(&self, order_id: OrderId) -> &[OrderLineId] {
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

    pub(in crate::app) fn line(&self, line_id: OrderLineId) -> Option<LineRecord> {
        self.lines.get(&line_id).cloned()
    }

    /// Folds one fire's consumption into the stock projection.
    ///
    /// A movement whose running total would leave `i64` is dropped with a warning rather than
    /// failing the fire: the sale is already committed and durable at this point, and refusing to
    /// update a cache is not a reason to tell the kitchen a line did not fire.
    pub(in crate::app) fn consume(&mut self, movements: &[StockMovement]) {
        for movement in movements {
            if let Err(error) = self.stock.apply(movement) {
                tracing::warn!(
                    target: LOG_TARGET,
                    %error,
                    "a stock movement overflowed the projection; not folded"
                );
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
    pub(in crate::app) fn on_hand(&self, ingredient: IngredientId) -> Quantity {
        self.stock.on_hand(ingredient)
    }

    pub(in crate::app) fn set_line_state(&mut self, line_id: OrderLineId, state: OrderLineState) {
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
    pub(in crate::app) fn set_fired_time(
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
    pub(in crate::app) fn fold_fired(
        &mut self,
        line_id: OrderLineId,
        fired_time: Timestamp,
        station_id: StationId,
    ) {
        self.set_line_state(line_id, OrderLineState::Fired);
        self.set_fired_time(line_id, fired_time, station_id);
    }

    /// Marks an item sold out, or restores it — from a live mark and from the fold on rebuild.
    pub(in crate::app) fn set_sold_out(&mut self, menu_item_id: MenuItemId, sold_out: bool) {
        if sold_out {
            self.sold_out.insert(menu_item_id);
        } else {
            self.sold_out.remove(&menu_item_id);
        }
    }

    /// Marks lines bumped (prepared) — from a live `bump_ticket` and from the fold on rebuild.
    pub(in crate::app) fn mark_bumped(&mut self, order_line_ids: &[OrderLineId]) {
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
    pub(in crate::app) fn bumped_line_ids(&self) -> Vec<OrderLineId> {
        let mut ids: Vec<OrderLineId> = self.bumped_lines.iter().copied().collect();
        ids.sort_unstable();
        ids
    }

    /// The lines on an order, in id order so the assembly (and its rounding residual) is
    /// deterministic across runs regardless of the hash map's iteration order.
    pub(in crate::app) fn lines_for_order(&self, order_id: OrderId) -> Vec<LineRecord> {
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
    pub(in crate::app) fn identified_lines_for_order(
        &self,
        order_id: OrderId,
    ) -> Vec<(OrderLineId, LineRecord)> {
        self.lines_where(order_id, |_| true)
    }

    /// Every **counter** order still owing money, in id order (so the counter list is stable across
    /// refreshes rather than reshuffling with the hash map).
    ///
    /// A counter order is one with lines and no table: a relayed marketplace order, the public API,
    /// a takeaway guest ([ADR-0064](../../../../../docs/adr/0064-edge-order-in.md)). Floor orders are
    /// excluded because the floor screen already shows them — listing a table's order here would
    /// give staff two places to charge one meal.
    ///
    /// "Still owing" means no bill, or a bill that has not settled. A settled order is finished; a
    /// bill already open is *not* finished and stays on the list, because the cashier who opened it
    /// still has to take the money and the screen must be able to resume rather than start again.
    pub(in crate::app) fn open_counter_orders(&self) -> Vec<OrderId> {
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
    pub(in crate::app) fn open_orders(&self) -> Vec<OrderId> {
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
    pub(in crate::app) fn oldest_open_work(&self, session: &EdgeSession) -> Option<u64> {
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
    pub(in crate::app) fn unfired_lines_for_order(
        &self,
        order_id: OrderId,
    ) -> Vec<(OrderLineId, LineRecord)> {
        self.lines_where(order_id, |record| record.state == OrderLineState::Added)
    }

    /// Applies a new quantity and extended total to a line the order still holds.
    ///
    /// Silent on a line this projection does not know, like every other setter here: a replay that
    /// reaches an update before its add is a replay out of order, and refusing would stop a box
    /// trading over a line that will exist a moment later.
    pub(in crate::app) fn set_line_quantity(
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
}
