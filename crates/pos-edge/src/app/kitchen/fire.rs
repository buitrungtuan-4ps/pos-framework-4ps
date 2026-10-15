// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! Firing a line, a whole order or one course to the kitchen: routing each line to its station and
//! consuming its recipe's stock.

use pos_core::capability::Capability;
use pos_core::decision::{Actor, LineCommand, decide_line};
use pos_ports::event_store::EventStore;
use pos_proto::events::SalesOrderLineFired;
use pos_proto::ids::{CourseId, OrderId, OrderLineId, StationId};

use crate::app::{AppError, Edge, FiredLine, LineView, StaffStanding};

impl<S: EventStore> Edge<S> {
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

        // The guardrail ADR-0012 and ADR-0057 promised, enforced for the first time here
        // (ADR-0116). Until this check existed `awaiting_staff_confirmation` had three readers and
        // all three only *reported* it — a guest's order reached the kitchen the moment anyone
        // pressed fire, exactly as an order with no hold would. Fire is the one gate: the order
        // stays addable, billable and settleable, because what staff have not agreed to is making
        // it.
        match self
            .lock_projection()
            .staff_standing(record.order_id, &session)
        {
            StaffStanding::Waiting => return Err(AppError::AwaitingStaffConfirmation),
            StaffStanding::Refused => return Err(AppError::OrderRejected),
            StaffStanding::Fireable => {}
        }

        // The published routing decides the station; the caller's station is only a fallback for a
        // store that has not published a station plan yet (never-blank, ADR-0072).
        let station_id = session
            .resolve_station(record.menu_item_id, record.course_id)
            .or(station_id)
            .ok_or(AppError::UnroutableLine)?;

        // The modifiers the line was added with, not an empty list. This argument was
        // `Vec::new()` until roadmap B1.1: `consumption_for_fire` deducts the base recipe plus one
        // recipe per modifier, so an empty list meant every modifier's ingredients were sold and
        // never taken off the shelf — the stock ledger drifted by exactly the modifiers, silently,
        // on every fired line.
        let command = LineCommand::Fire {
            base_item: record.menu_item_id,
            modifiers: record.modifier_menu_item_ids.clone(),
            quantity: record.quantity,
            // `None`, and not the line's own course. The field says which course is *being fired*
            // — it is what `courses_enabled` gates — not which one the line belongs to. Passing the
            // taxonomy here made a store with courses off unable to fire a line that merely
            // belonged to a course, and nothing gates adding one, so the food had no way to the
            // kitchen short of a void. The station lookup below still reads the line's course,
            // because *where* the food goes is a different question from *when* it goes.
            course: None,
        };
        let decision = decide_line(record.state, command, &ctx, &session.recipes)?;

        let payload = SalesOrderLineFired {
            order_id: record.order_id,
            order_line_id,
            station_id,
            fire_time: ctx.now,
        };
        self.commit_and_publish(&ctx, &payload).await?;

        {
            let mut projection = self.lock_projection();
            projection.set_line_state(order_line_id, decision.next_state);
            // The same `ctx.now` the event carries, so a board reading the projection and a board
            // rebuilt from the log show the same age.
            projection.set_fired_time(order_line_id, ctx.now, station_id);
            // §8: stock leaves at fire, not at payment. Folded here rather than discarded — see
            // `Projection::stock` for what this figure is and is not yet.
            projection.consume(&decision.stock_movements);
        }
        Ok(LineView {
            order_id: record.order_id,
            order_line_id,
            state: decision.next_state,
            fired: Some(FiredLine {
                station_id,
                menu_item_id: record.menu_item_id,
                quantity: record.quantity,
                modifier_menu_item_ids: record.modifier_menu_item_ids,
                note_present: record.note_present,
            }),
        })
    }

    /// Sends every unsent line on an order to the kitchen, in one transaction.
    ///
    /// The operator's act is "send this order", and it was costing one tap and one round trip per
    /// line: a table of six was six taps, and a failure halfway left three lines with the kitchen
    /// and three not, with nothing on the screen saying which. This commits every line's
    /// `sales.order_line.fired` together or none of them, so the kitchen is told about an order the
    /// same way an operator thinks about one.
    ///
    /// Each line is still decided **individually** by `decide_line` and routed individually by the
    /// published station plan (ADR-0072) — a batch is a transaction boundary, never a shortcut past
    /// a decision. The staff-confirmation gate (ADR-0116) is asked once, because it is a fact about
    /// the order rather than about a line.
    ///
    /// An order with nothing unsent is not an error: it returns an empty list. Two devices tapping
    /// send on the same table is an ordinary race, and the second one finding the work done is the
    /// right outcome, not a refusal to explain to whoever tapped.
    ///
    /// # Errors
    ///
    /// [`AppError::Domain`] if a line refuses the fire, [`AppError::UnroutableLine`] if a line
    /// reaches no station, and the confirmation errors when a guest's order is still waiting on
    /// staff. An order this edge has never heard of is not one of them: it has nothing unsent, and
    /// so is answered the same way an order already sent is.
    pub async fn fire_order(
        &self,
        actor: Actor,
        order_id: OrderId,
        station_id: Option<StationId>,
    ) -> Result<Vec<LineView>, AppError> {
        self.fire_unsent(actor, order_id, station_id, None).await
    }

    /// Sends every unsent line **on one course** to the kitchen, in one transaction (ADR-0130).
    ///
    /// The other half of the same operator act: "starters away" rather than "send the order". It is
    /// [`Self::fire_order`] narrowed to one course, and deliberately the same routine — a second
    /// copy would be a second place for the transaction boundary, the routing and the staff gate to
    /// drift, and those are exactly the properties that must not differ between the two.
    ///
    /// **This is what the `courses_enabled` capability gates.** `LineCommand::Fire { course }` means
    /// the line is being fired as part of a course, and until now nothing filled it — the gate had a
    /// domain test and no production caller. A store with courses off is refused here, before the
    /// empty-list shortcut below, so asking is refused rather than quietly answered with "nothing to
    /// do" on an order whose starters are all still waiting.
    ///
    /// A course with nothing unsent on it returns an empty list, for [`Self::fire_order`]'s reason:
    /// two devices sending the starters is an ordinary race and the second one finding the work done
    /// is the right outcome. An order that never had a line on that course is answered the same way,
    /// because telling the two apart would need the catalog and would turn a race into a refusal.
    ///
    /// # Errors
    ///
    /// [`DomainError::CapabilityDisabled`](pos_core::error::DomainError::CapabilityDisabled) when
    /// the store has courses off, plus everything [`Self::fire_order`] can return.
    pub async fn fire_course(
        &self,
        actor: Actor,
        order_id: OrderId,
        course_id: CourseId,
        station_id: Option<StationId>,
    ) -> Result<Vec<LineView>, AppError> {
        self.fire_unsent(actor, order_id, station_id, Some(course_id))
            .await
    }

    /// The one routine both fire paths are: every unsent line on an order, or every unsent line on
    /// one of its courses, committed together or not at all.
    async fn fire_unsent(
        &self,
        actor: Actor,
        order_id: OrderId,
        station_id: Option<StationId>,
        course: Option<CourseId>,
    ) -> Result<Vec<LineView>, AppError> {
        let ctx = self.decision_ctx(actor)?;
        let session = self.session();

        // Before the empty-list shortcut, so a store with courses off is told it cannot fire by
        // course rather than told there is nothing to fire. `decide_line` would refuse it too, but
        // only once a line got that far, and an empty course never would.
        if course.is_some() {
            ctx.require_capability(Capability::Courses)?;
        }

        // Asked once, for the order. A guest's tabled QR order that nobody has confirmed cannot
        // reach the kitchen, and doing this per line would ask the same question six times.
        match self.lock_projection().staff_standing(order_id, &session) {
            StaffStanding::Waiting => return Err(AppError::AwaitingStaffConfirmation),
            StaffStanding::Refused => return Err(AppError::OrderRejected),
            StaffStanding::Fireable => {}
        }

        let mut unfired = self.lock_projection().unfired_lines_for_order(order_id);
        if let Some(course) = course {
            unfired.retain(|(_, record)| record.course_id == Some(course));
        }
        if unfired.is_empty() {
            return Ok(Vec::new());
        }

        let mut envelopes = Vec::with_capacity(unfired.len());
        let mut messages = Vec::with_capacity(unfired.len());
        let mut applied = Vec::with_capacity(unfired.len());
        for (order_line_id, record) in unfired {
            let station_id = session
                .resolve_station(record.menu_item_id, record.course_id)
                .or(station_id)
                .ok_or(AppError::UnroutableLine)?;
            let command = LineCommand::Fire {
                base_item: record.menu_item_id,
                modifiers: record.modifier_menu_item_ids.clone(),
                quantity: record.quantity,
                // The course being fired, not the one the line belongs to: `None` from
                // `fire_order`, because firing everything is not firing by course, and it costs
                // more here than on the single-line path — one refusal aborts the batch, so one
                // line on a course would make the whole order unfireable.
                course,
            };
            let decision = decide_line(record.state, command, &ctx, &session.recipes)?;
            let (envelope, message) = self.prepare(
                &ctx,
                &SalesOrderLineFired {
                    order_id: record.order_id,
                    order_line_id,
                    station_id,
                    fire_time: ctx.now,
                },
            )?;
            envelopes.push(envelope);
            messages.push(message);
            applied.push((order_line_id, record, station_id, decision));
        }

        // Nothing above has written or told anybody: a refusal on the last line leaves the first
        // five unsent, which is the whole point of doing this in one transaction.
        self.append_and_publish(envelopes, messages).await?;

        let mut views = Vec::with_capacity(applied.len());
        {
            let mut projection = self.lock_projection();
            for (order_line_id, record, station_id, decision) in applied {
                projection.set_line_state(order_line_id, decision.next_state);
                // Per line, as the single-line path does, and from the same `ctx.now` those lines'
                // events carry: one "send to kitchen" fires its lines together, so they age together.
                projection.set_fired_time(order_line_id, ctx.now, station_id);
                // §8: stock leaves at fire, not at payment — per line, as the single-line path does.
                projection.consume(&decision.stock_movements);
                views.push(LineView {
                    order_id: record.order_id,
                    order_line_id,
                    state: decision.next_state,
                    fired: Some(FiredLine {
                        station_id,
                        menu_item_id: record.menu_item_id,
                        quantity: record.quantity,
                        modifier_menu_item_ids: record.modifier_menu_item_ids,
                        note_present: record.note_present,
                    }),
                });
            }
        }
        Ok(views)
    }
}
