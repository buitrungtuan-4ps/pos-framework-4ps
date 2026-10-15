// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! Taking a line onto an order: its state, its modifiers checked against the menu, and adding it,
//! with or without a guest's note, to a table's order or to an order by id.

use pos_core::capability::Capability;
use pos_core::decision::{Actor, DecisionCtx};
use pos_core::menu::{RequestedLine, reprice_line};
use pos_core::permission::Permission;
use pos_ports::event_store::EventStore;
use pos_proto::OrderLineState;
use pos_proto::events::SalesOrderLineAdded;
use pos_proto::ids::{MenuItemId, OrderId, OrderLineId, TableId};
use pos_proto::menu::MenuCatalog;
use pos_proto::text::DisplayName;

use crate::app::records::attach_note;
use crate::app::{AppError, Edge, LineDraft, LineRecord, LineView, OrderLineChoice, StaffDecision};
use crate::line_notes::NoteText;

impl<S: EventStore> Edge<S> {
    /// The current state of a line, if the edge knows it.
    #[must_use]
    pub fn line_state(&self, order_line_id: OrderLineId) -> Option<OrderLineState> {
        self.lock_projection().line(order_line_id).map(|r| r.state)
    }

    /// Whether a line's chosen modifiers satisfy every group its item attaches (ADR-0127).
    ///
    /// Two different mistakes, checked separately because they send an operator to two different
    /// places: a **group's rule broken** (no size chosen on a pizza that needs one, three toppings
    /// where two is the maximum), and a **modifier that belongs to no attached group** — a device
    /// sending an arbitrary item id as a modifier, which would otherwise be charged for and made.
    ///
    /// A store that has published no groups reaches the empty loop and the empty difference, so
    /// every line on it costs one lookup that finds nothing. That is the common case today and it
    /// stays free.
    fn check_modifiers(menu: &MenuCatalog, draft: &LineDraft) -> Result<(), AppError> {
        let groups = menu.groups_for(draft.menu_item_id);
        // **An item that attaches no group is unconfigured, not empty-handed.** `add_line` has
        // accepted `modifier_menu_item_ids` since long before groups existed, and every store is
        // publishing none today — so enforcing "a modifier must be offered by a group" here would
        // refuse lines that work now, on every store, until the console got round to attaching one.
        // The edge checks what has been published and nothing else; the pos-edge suite caught this
        // by failing `a_fired_line_consumes_its_modifier_recipe_as_well_as_the_base`, which prices
        // a modifier on a catalogue with no groups in it at all.
        if groups.is_empty() {
            return Ok(());
        }
        for group in &groups {
            if !group.satisfied_by(&draft.modifier_menu_item_ids) {
                return Err(AppError::ModifierSelectionInvalid);
            }
        }
        // Once an item *does* attach groups, every modifier on it must come from one of them.
        // Without this a line could name any item id at all — a device's typo, or worse — and the
        // kitchen would be told to add it and the guest charged for it.
        let offered = |chosen: &MenuItemId| {
            groups
                .iter()
                .any(|group| group.member_menu_item_ids.contains(chosen))
        };
        if !draft.modifier_menu_item_ids.iter().all(offered) {
            return Err(AppError::ModifierSelectionInvalid);
        }
        Ok(())
    }

    /// The names `menu` gives a line's modifiers, index-aligned with `ids`
    /// ([ADR-0144](../../../docs/adr/0144-a-line-records-the-names-of-its-modifiers.md)) — or none
    /// at all if the book cannot name every one.
    ///
    /// All or nothing, because the event promises the two lists line up by position, and one gap
    /// would pair every later name with the wrong modifier. A book that cannot name a modifier it
    /// offers is a publishing fault; that line's receipt prints no modifiers, as a line recorded
    /// before ADR-0144 does, and the kitchen ticket still falls back to the id.
    fn modifier_names(menu: &MenuCatalog, ids: &[MenuItemId]) -> Vec<DisplayName> {
        ids.iter()
            .map(|id| menu.get(*id).map(|entry| entry.display_name.clone()))
            .collect::<Option<Vec<_>>>()
            .unwrap_or_default()
    }

    /// Adds a line to the order the table holds (`sales.order_line.added`).
    ///
    /// Adding a line is not a state-machine transition; it needs `sales.line.add` (ADR-0158) and
    /// records what the device captured. A new line starts [`OrderLineState::Added`].
    ///
    /// **The modifiers are checked**, and the device is not trusted to have asked (ADR-0127
    /// decision 5). A line names `modifier_menu_item_ids`; the item it names attaches zero or more
    /// groups, each with a `min_select`/`max_select` rule. A selection that breaks one — or that
    /// names a modifier belonging to no group this item attaches — is refused. A required choice a
    /// till that forgot to ask can skip is required in name only, and by the time the kitchen reads
    /// the ticket the line has already been priced without the size it was sold at.
    ///
    /// A line naming no modifiers on an item attaching no groups is every line on every store that
    /// has published none, and costs it a lookup that finds nothing.
    ///
    /// **A seat is the other thing checked**, because it is the one thing here a store can turn off.
    /// `Capability::Seats` is off by default — most counters have no seats — and a flag nothing
    /// enforces is decoration: a device that asked for a seat anyway would write one into the log of
    /// a store that does not do seats, and the by-seat split that reads it later would find guests at
    /// a table nobody seated. A line with no seat is untouched either way, which is every line on
    /// every store that has not turned this on.
    ///
    /// # Errors
    ///
    /// [`AppError::NoOpenOrder`] if the table has not been seated, [`AppError::BillAlreadyOpen`] once
    /// a bill is open on its order (that bill would not cover the line), [`AppError::Domain`] if the
    /// actor lacks `sales.line.add` or the line names a seat and the store does not do seats,
    /// [`AppError::ModifierSelectionInvalid`] if the modifiers break the item's rules, or
    /// [`AppError`] if the store cannot be written.
    pub async fn add_line(
        &self,
        actor: Actor,
        table_id: TableId,
        draft: LineDraft,
    ) -> Result<LineView, AppError> {
        self.add_noted_line(actor, table_id, draft, None).await
    }

    /// [`Self::add_line`], with the guest's note for the kitchen
    /// ([ADR-0157](../../../docs/adr/0157-a-guest-note-lives-in-the-stores-memory-for-the-service.md)).
    ///
    /// The event records only that a note exists. The text is held in memory for as long as the
    /// line is on a screen, printed on its ticket, and written nowhere.
    ///
    /// # Errors
    ///
    /// As [`Self::add_line`].
    pub async fn add_noted_line(
        &self,
        actor: Actor,
        table_id: TableId,
        draft: LineDraft,
        note: Option<NoteText>,
    ) -> Result<LineView, AppError> {
        let ctx = self.decision_ctx(actor)?;
        ctx.require(Permission::AddLine)?;
        if draft.seat.is_some() {
            ctx.require_capability(Capability::Seats)?;
        }
        // An item staff have marked sold out cannot be ordered, whatever a device still showing it
        // sends: the till greys it out, and this is where that is enforced.
        if self
            .first_sold_out(draft.menu_item_id, &draft.modifier_menu_item_ids)
            .is_some()
        {
            return Err(AppError::ItemNotSellable);
        }
        let session = self.session();
        Self::check_modifiers(&session.menu, &draft)?;
        // The device priced this line from the session's price book and sends the modifiers' ids,
        // never their names, so the names come from that book (ADR-0144).
        let modifier_display_names =
            Self::modifier_names(&session.menu, &draft.modifier_menu_item_ids);
        let (order_id, billed) = {
            let projection = self.lock_projection();
            let order_id = projection.order_for_table(table_id);
            let billed = order_id.and_then(|id| projection.bill_for_order(id));
            (order_id, billed.is_some())
        };
        let order_id = order_id.ok_or(AppError::NoOpenOrder)?;
        // A bill names the lines it covers when it opens (ADR-0128 decision 9), so a line rung
        // after it is on no bill: the guest pays the old total and the line leaves with the table,
        // unpaid. The counter path has refused this since ADR-0146; the floor refuses it too, and
        // the way to order more is to void the bill, whose replacement covers every line.
        if billed {
            return Err(AppError::BillAlreadyOpen);
        }
        self.append_line(&ctx, order_id, draft, modifier_display_names, note)
            .await
    }

    /// Adds a line to an order by its id, **priced by the edge** at the order's own channel
    /// ([ADR-0146](../../../docs/adr/0146-a-counter-store-starts-its-own-orders.md)).
    ///
    /// The table path records the prices the device copied from the menu, which is priced for the
    /// session's channel. A takeaway order carrying dine-in prices but taxed at the takeaway rate
    /// would be a receipt that does not add up, so this path looks the item up itself, as the order
    /// intake does, and the device sends only what the guest chose.
    ///
    /// # Errors
    ///
    /// [`AppError::UnknownOrder`] for an order this edge never saw open, [`AppError::OrderRejected`]
    /// for one staff refused, [`AppError::BillAlreadyOpen`] once a bill is open on it (that bill
    /// would not cover the line), [`AppError::ItemNotSellable`] if the price book cannot price it,
    /// [`AppError::ModifierSelectionInvalid`], [`AppError::Domain`] if the actor lacks
    /// `sales.line.add` or for a seat on a store without seats, or [`AppError`] if the store cannot
    /// be written.
    pub async fn add_line_to_order(
        &self,
        actor: Actor,
        order_id: OrderId,
        choice: OrderLineChoice,
    ) -> Result<LineView, AppError> {
        self.add_noted_line_to_order(actor, order_id, choice, None)
            .await
    }

    /// [`Self::add_line_to_order`], with the guest's note for the kitchen (ADR-0157), as
    /// [`Self::add_noted_line`] is for a table.
    ///
    /// # Errors
    ///
    /// As [`Self::add_line_to_order`].
    pub async fn add_noted_line_to_order(
        &self,
        actor: Actor,
        order_id: OrderId,
        choice: OrderLineChoice,
        note: Option<NoteText>,
    ) -> Result<LineView, AppError> {
        let ctx = self.decision_ctx(actor)?;
        ctx.require(Permission::AddLine)?;
        if choice.seat.is_some() {
            ctx.require_capability(Capability::Seats)?;
        }
        if self
            .first_sold_out(choice.menu_item_id, &choice.modifier_menu_item_ids)
            .is_some()
        {
            return Err(AppError::ItemNotSellable);
        }
        let session = self.session();
        let (origin, bill, refused) = {
            let projection = self.lock_projection();
            (
                projection.order_origin(order_id),
                projection.bill_for_order(order_id),
                projection.staff_decision(order_id) == Some(StaffDecision::Rejected),
            )
        };
        let origin = origin.ok_or(AppError::UnknownOrder)?;
        if refused {
            return Err(AppError::OrderRejected);
        }
        if bill.is_some() {
            return Err(AppError::BillAlreadyOpen);
        }
        let channel = origin.channel.unwrap_or(session.sales_channel);
        // The order's own channel's price book, not the store's: a takeaway order is priced at
        // takeaway prices, as it is taxed at the takeaway rate.
        let menu = session.menu_for(channel);
        let requested = RequestedLine {
            menu_item_id: choice.menu_item_id,
            quantity: choice.quantity,
            modifier_menu_item_ids: choice.modifier_menu_item_ids,
            quoted_unit_price: None,
        };
        let priced = reprice_line(menu, &session.tax_rates, channel, &requested)
            .map_err(AppError::from_reprice)?;
        let draft = LineDraft {
            menu_item_id: priced.menu_item_id,
            display_name: priced.display_name,
            quantity: priced.quantity,
            unit_price: priced.unit_price,
            line_total: priced.line_total,
            tax_class_id: priced.tax_class_id,
            tax_rate: priced.tax_rate,
            seat: choice.seat,
            course_id: choice.course_id,
            modifier_menu_item_ids: priced.modifier_menu_item_ids,
            note_present: choice.note_present,
        };
        Self::check_modifiers(menu, &draft)?;
        self.append_line(&ctx, order_id, draft, priced.modifier_display_names, note)
            .await
    }

    /// Records a line on an order and folds it into the projection: the shared tail of the table
    /// path and the order path.
    ///
    /// `modifier_display_names` is the caller's, because each path priced the line against its own
    /// book: the device's copy of the session's menu, or the edge's repricing (ADR-0144).
    ///
    /// A note is held before the write and forgotten if the write fails, so a device that reads the
    /// live orders the moment the frame reaches it finds the note already there (ADR-0157).
    async fn append_line(
        &self,
        ctx: &DecisionCtx,
        order_id: OrderId,
        draft: LineDraft,
        modifier_display_names: Vec<DisplayName>,
        note: Option<NoteText>,
    ) -> Result<LineView, AppError> {
        let order_line_id = OrderLineId::new(self.next_ulid());
        // A device that still sends only the flag keeps working; a note implies it.
        let note_present = draft.note_present || note.is_some();

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
            modifier_menu_item_ids: draft.modifier_menu_item_ids.clone(),
            modifier_display_names,
            note_present,
        };
        let (envelope, mut message) = self.prepare(ctx, &payload)?;
        if let Some(note) = note {
            attach_note(&mut message, &note);
            self.lock_notes().hold(order_id, order_line_id, note);
        }
        if let Err(error) = self.append_and_publish(vec![envelope], vec![message]).await {
            self.lock_notes().forget(order_line_id);
            return Err(error);
        }

        // From the event, for the reason the order-in path above says.
        self.lock_projection()
            .add_line(order_line_id, LineRecord::from(payload));
        Ok(LineView {
            order_id,
            order_line_id,
            state: OrderLineState::Added,
            // Nothing is made until a line fires, so an added line has no ticket.
            fired: None,
        })
    }
}

#[cfg(test)]
mod tests {
    use pos_proto::ids::TableId;
    use pos_proto::ulid::Ulid;

    use crate::app::tests::{a_line, actor, edge};

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
}
