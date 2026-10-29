// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! Writing and publishing events: preparing each envelope, committing and appending them, then
//! publishing to every device; the guest notes held beside the projection, the locks, and the ids
//! a command mints.

use pos_core::decision::DecisionCtx;
use pos_ports::TxContext;
use pos_ports::event_store::EventStore;
use pos_proto::envelope::{EventEnvelope, EventPayload, EventTypeRef, RawPayload};
use pos_proto::ids::{DeviceId, OrderId, OrderLineId, ShiftId};
use pos_proto::time::{BusinessDate, Timestamp};
use pos_proto::ulid::Ulid;
use pos_proto::{IdGenerator, OrderLineState};

use crate::app::{AppError, Edge, Projection};
use crate::fanout::ServerMessage;
use crate::line_notes::{LineNotes, NoteText};

impl<S: EventStore> Edge<S> {
    /// The commit-and-publish half of a single-event command: write the event in one transaction,
    /// then — and only then — tell every device. In that order, so a rolled-back write is never seen.
    /// Updating the projection is the caller's, done after this returns for the same reason.
    pub(in crate::app) async fn commit_and_publish<P: EventPayload + serde::Serialize>(
        &self,
        ctx: &DecisionCtx,
        payload: &P,
    ) -> Result<(), AppError> {
        let (envelope, message) = self.prepare(ctx, payload)?;
        self.append_and_publish(vec![envelope], vec![message]).await
    }

    /// Prepares an event for the store and the fan-out: the stamped envelope to append, and the
    /// message to broadcast once it commits.
    pub(in crate::app) fn prepare<P: EventPayload + serde::Serialize>(
        &self,
        ctx: &DecisionCtx,
        payload: &P,
    ) -> Result<(EventEnvelope<RawPayload>, ServerMessage), AppError> {
        self.prepare_on(ctx, payload, self.current_shift_id())
    }

    /// [`Self::prepare`], stamped with `shift` rather than the store's drawer's shift: what an act
    /// on one drawer's shift is stamped with (ADR-0167 decision 5).
    pub(in crate::app) fn prepare_on<P: EventPayload + serde::Serialize>(
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
    pub(in crate::app) fn system_prepare<P: EventPayload + serde::Serialize>(
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
    pub(in crate::app) async fn append_and_publish(
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
    pub(in crate::app) fn lock_notes(&self) -> std::sync::MutexGuard<'_, LineNotes> {
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
    pub(in crate::app) fn forget_notes_off_screen(&self) {
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
    pub(in crate::app) fn lock_projection(&self) -> std::sync::MutexGuard<'_, Projection> {
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

    pub(in crate::app) fn next_ulid(&self) -> Ulid {
        self.ids
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .next_id()
    }

    pub(in crate::app) fn next_order_id(&self) -> OrderId {
        OrderId::new(self.next_ulid())
    }
}
