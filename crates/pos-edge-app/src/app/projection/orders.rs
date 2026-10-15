// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! What the projection records of tables and orders: the table an order is at, where an order came
//! from and who confirmed it, opening, closing, joining and merging an order.

use pos_proto::ids::{OrderId, OrderLineId, TableId};
use pos_proto::{BillState, SalesChannel, TableState};

use crate::app::{EdgeSession, LineRecord, OrderOrigin, Projection, StaffDecision, StaffStanding};

impl Projection {
    pub(in crate::app) fn table_state(&self, table_id: TableId) -> TableState {
        self.tables
            .get(&table_id)
            .copied()
            .unwrap_or(TableState::Free)
    }

    pub(in crate::app) fn set_table(&mut self, table_id: TableId, state: TableState) {
        self.tables.insert(table_id, state);
    }

    pub(in crate::app) fn open_order(&mut self, table_id: TableId, order_id: OrderId) {
        self.table_orders.insert(table_id, order_id);
    }

    pub(in crate::app) fn order_for_table(&self, table_id: TableId) -> Option<OrderId> {
        self.table_orders.get(&table_id).copied()
    }

    /// The table an order sits on, if any — the reverse of [`Self::open_order`], used by rebuild to
    /// recover a bill's table from the log (a bill event names its order, not its table).
    pub(in crate::app) fn table_for_order(&self, order_id: OrderId) -> Option<TableId> {
        self.table_orders
            .iter()
            .find(|(_, order)| **order == order_id)
            .map(|(table, _)| *table)
    }

    /// Moves an order from the table its guests left to the one they sat down at
    /// (`sales.table.transferred`), live and on rebuild. The tables' own states are the caller's:
    /// the live command has the machine's answer, and the fold reads what that answer always is.
    pub(in crate::app) fn move_order(
        &mut self,
        order_id: OrderId,
        from_table_id: TableId,
        to_table_id: TableId,
    ) {
        if self.order_for_table(from_table_id) == Some(order_id) {
            self.table_orders.remove(&from_table_id);
        }
        self.open_order(to_table_id, order_id);
        if let Some(origin) = self.orders.get_mut(&order_id) {
            origin.table_id = Some(to_table_id);
        }
    }

    /// Records where an order came from, from a live open or from the fold on rebuild.
    pub(in crate::app) fn open_order_origin(
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
    pub(in crate::app) fn order_channel(&self, order_id: OrderId) -> Option<SalesChannel> {
        self.orders.get(&order_id).and_then(|origin| origin.channel)
    }

    /// Where an order came from, if this build saw it open.
    pub(in crate::app) fn order_origin(&self, order_id: OrderId) -> Option<OrderOrigin> {
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
    pub(in crate::app) fn record_staff_decision(
        &mut self,
        order_id: OrderId,
        decision: StaffDecision,
    ) {
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
    pub(in crate::app) fn seat_guest_order(&mut self, table_id: TableId, order_id: OrderId) {
        self.set_table(table_id, TableState::Occupied);
        self.open_order(table_id, order_id);
    }

    /// What staff decided about an order, or `None` while it is still theirs to decide.
    pub(in crate::app) fn staff_decision(&self, order_id: OrderId) -> Option<StaffDecision> {
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
    pub(in crate::app) fn awaits_staff_confirmation(
        &self,
        order_id: OrderId,
        session: &EdgeSession,
    ) -> bool {
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
    pub(in crate::app) fn staff_standing(
        &self,
        order_id: OrderId,
        session: &EdgeSession,
    ) -> StaffStanding {
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
    pub(in crate::app) fn orders_awaiting_staff_confirmation(
        &self,
        session: &EdgeSession,
    ) -> Vec<OrderId> {
        let mut held: Vec<OrderId> = self
            .orders
            .keys()
            .copied()
            .filter(|order_id| self.awaits_staff_confirmation(*order_id, session))
            .collect();
        held.sort_unstable();
        held
    }

    pub(in crate::app) fn add_line(&mut self, line_id: OrderLineId, record: LineRecord) {
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
    pub(in crate::app) fn close_order(&mut self, order_id: OrderId) {
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
    pub(in crate::app) fn fold_inbound_order(
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
    pub(in crate::app) fn merge_order(&mut self, target: OrderId, merged: OrderId) {
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
    pub(in crate::app) fn joinable_order_at(
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
}
