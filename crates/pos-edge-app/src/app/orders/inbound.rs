// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! An order that arrives from outside the store ([ADR-0064](../../../../../docs/adr/0064-edge-order-in.md)):
//! a marketplace order, the public API or a QR guest, opened with its already-priced lines, joined
//! to its table's order where the store says so, and looked up again by its idempotency key.

use pos_core::business_date::derive_business_date;
use pos_core::menu::PricedLine;
use pos_ports::TxContext;
use pos_ports::event_store::EventStore;
use pos_ports::intake_ledger::{IntakeLedger, IntakeRecord};
use pos_proto::envelope::{EventEnvelope, RawPayload};
use pos_proto::events::{SalesOrderLineAdded, SalesOrderOpened, SalesTableMerged};
use pos_proto::ids::{DeviceId, OrderId, OrderLineId, TableId};
use pos_proto::qr::TableOrder;
use pos_proto::time::{BusinessDate, Timestamp};
use pos_proto::{ClockSource, Open, SalesChannel};

use crate::app::records::attach_note;
use crate::app::{AppError, Edge, EdgeSession, InboundOrderOpened, IntakeIntent, LineRecord};
use crate::fanout::ServerMessage;
use crate::line_notes::NoteText;

impl<S: EventStore> Edge<S> {
    /// Opens an order that arrived from **outside** the store — a marketplace order, the public API,
    /// or a QR guest ([ADR-0064](../../../../../docs/adr/0064-edge-order-in.md)). Emits `sales.order.opened`
    /// (tableless-capable) and one `sales.order_line.added` per already-priced line, in **one**
    /// transaction, then folds them into the projection; returns the new order's id and the business
    /// date it was stamped with — the caller keys the daily queue number on that same date so the
    /// number belongs to the trading day the order actually opened on.
    ///
    /// Unlike the floor commands there is no signed-in employee, so the events carry the box's own
    /// `device_id` and no employee — the same shape [`Self::record_activation`] uses. The lines are
    /// already priced by the caller (`pos_core::menu::reprice_line`); this records what it decided.
    ///
    /// # A guest's order that joins the table's
    ///
    /// Where the store sets `qr.table_order` to `TABLE_ORDER_JOIN` (ADR-0160 item 2), a QR order for
    /// a table whose order can take a line, and that waits for no member of staff, joins that order
    /// in the same transaction: `sales.table.merged` follows its lines, which are the table's
    /// order's from then on. The guest's order is still opened, and its own id is the one returned
    /// and recorded, so each line keeps the order, and through it the channel, it was ordered on,
    /// and each submission keeps an order of its own for its retries to find. A QR order that waits
    /// for staff joins the table's when they confirm it ([`Self::confirm_inbound_order`]). A table
    /// whose order cannot take a line, because its bill is open or paid, keeps the guest's order
    /// separate, as before.
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
        lines: &[(PricedLine, Option<NoteText>)],
        intake: Option<IntakeIntent<'_>>,
    ) -> Result<InboundOrderOpened, AppError>
    where
        S: IntakeLedger,
    {
        // A replaced machine takes no new channel order (ADR-0123). Refused *before* the
        // idempotency row is written, so the delivery is left un-acknowledged and the cloud can
        // hand it to the replacement — see `crate::order_in`, which maps this to `unavailable`
        // rather than to a failure, because an order the guest has already paid for must not be
        // dropped because the shop swapped a machine.
        self.refuse_if_superseded()?;
        let now = self.clock.now();
        let session = self.session();
        let stamp = system_stamp(device_id, now, &session)?;

        // Read the channel out before the event takes ownership: the projection needs it too, so
        // the bill is taxed at this order's rate rather than the session's (B1.2).
        let order_channel = channel.require().ok();

        // Asked once, here, from the session snapshot this order was priced against — and handed
        // back to the caller below rather than recomputed there. The caller holds its own snapshot
        // taken microseconds earlier, and the config-pull loop can swap the session between the
        // two; recomputing would reintroduce the disagreement this replaces, just more rarely.
        let holds_for_staff = session.holds_for_staff_confirmation(order_channel, table_id);
        let joins =
            self.order_joined_on_arrival(&session, order_channel, table_id, holds_for_staff);

        let order_id = self.next_order_id();
        let opened = SalesOrderOpened {
            order_id,
            channel,
            table_id,
            guest_count: None,
        };
        let prepared = self.prepare_inbound(stamp, &opened, lines, joins)?;
        let (envelopes, messages): (Vec<_>, Vec<_>) = prepared.events.into_iter().unzip();

        let business_date = stamp.business_date;
        self.commit_inbound(order_id, business_date, holds_for_staff, &envelopes, intake)
            .await?;
        self.announce_inbound(order_id, prepared.notes, &messages);

        self.lock_projection().fold_inbound_order(
            order_id,
            (order_channel, table_id),
            prepared.records,
            joins,
        );
        Ok(InboundOrderOpened {
            order_id,
            business_date,
            awaiting_staff_confirmation: holds_for_staff,
        })
    }

    /// The inbound order's events, prepared in the order they are written: the order opened, one
    /// `sales.order_line.added` per already-priced line in the order the lines came, and the
    /// table's order it joins when it joins one — with each line's projection record and the
    /// notes to hold once the order commits.
    fn prepare_inbound(
        &self,
        stamp: SystemStamp,
        opened: &SalesOrderOpened,
        lines: &[(PricedLine, Option<NoteText>)],
        joins: Option<OrderId>,
    ) -> Result<PreparedOrder, AppError> {
        let SystemStamp {
            device_id,
            now,
            business_date,
        } = stamp;
        let order_id = opened.order_id;
        let mut prepared = PreparedOrder {
            // Each event with the frame that announces it once it commits, in the order they are
            // written.
            events: Vec::with_capacity(lines.len() + 2),
            records: Vec::with_capacity(lines.len()),
            notes: Vec::new(),
        };
        let opened_event = self.system_prepare(device_id, now, business_date, opened)?;
        prepared.events.push(opened_event);
        for (priced, note) in lines {
            let order_line_id = OrderLineId::new(self.next_ulid());
            let added = line_added(order_id, order_line_id, priced, note.is_some());
            let (envelope, mut message) =
                self.system_prepare(device_id, now, business_date, &added)?;
            if let Some(note) = note {
                attach_note(&mut message, note);
                prepared.notes.push((order_line_id, note.clone()));
            }
            prepared.events.push((envelope, message));
            // Built from the event rather than beside it. Restating the same fields in two
            // places is how the projection came to be missing `display_name` and `unit_price` in the
            // first place: the event grew them, the hand-written twin did not, and nothing noticed
            // because nothing yet asked for them. The replay already folds through this conversion,
            // so using it here makes the live path and the replay path the same code.
            prepared
                .records
                .push((order_line_id, LineRecord::from(added)));
        }
        // Last, so the log reads as it happened: the guest's order, its lines, and the order they
        // moved to. The same fold replays it.
        if let Some(target_order_id) = joins {
            let merged = SalesTableMerged {
                target_order_id,
                merged_order_id: order_id,
            };
            let merged_event = self.system_prepare(device_id, now, business_date, &merged)?;
            prepared.events.push(merged_event);
        }
        Ok(prepared)
    }

    /// The events AND the idempotency ledger row commit in ONE transaction, so a crash between
    /// opening the order and recording it is impossible — either both land or neither
    /// (ADR-0064). A plain insert on an existing key fails the commit with `already_exists` and
    /// rolls the events back with it, which is how a concurrent second order on the same key is
    /// refused rather than duplicated.
    async fn commit_inbound(
        &self,
        order_id: OrderId,
        business_date: BusinessDate,
        holds_for_staff: bool,
        envelopes: &[EventEnvelope<RawPayload>],
        intake: Option<IntakeIntent<'_>>,
    ) -> Result<(), AppError>
    where
        S: IntakeLedger,
    {
        let mut tx = self.store.begin().await?;
        self.store.append(&mut tx, envelopes).await?;
        if let Some(intent) = intake {
            let record = IntakeRecord {
                order_id,
                business_date,
                total: intent.total,
                repriced: intent.repriced,
                awaiting_staff_confirmation: holds_for_staff,
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
        Ok(())
    }

    /// Holds the lines' notes, then announces the committed order to the devices.
    ///
    /// Held after the commit: a delivery that loses the race to a duplicate must leave nothing
    /// behind, and nothing reads these lines until the frames announce them.
    fn announce_inbound(
        &self,
        order_id: OrderId,
        line_notes: Vec<(OrderLineId, NoteText)>,
        messages: &[ServerMessage],
    ) {
        {
            let mut notes = self.lock_notes();
            for (order_line_id, note) in line_notes {
                notes.hold(order_id, order_line_id, note);
            }
        }
        for message in messages {
            self.fanout.publish(message);
        }
    }

    /// The table's order a guest's order joins as it arrives (ADR-0160 item 2), if any: a QR order
    /// at a table, at a store that sets `qr.table_order` to `TABLE_ORDER_JOIN`, that waits for no
    /// member of staff. One that waits joins when staff confirm it
    /// ([`Self::confirm_inbound_order`]). Read before the write, as a waiter's line reads the bill
    /// it must not land after.
    fn order_joined_on_arrival(
        &self,
        session: &EdgeSession,
        channel: Option<SalesChannel>,
        table_id: Option<TableId>,
        holds_for_staff: bool,
    ) -> Option<OrderId> {
        let joins = channel == Some(SalesChannel::Qr)
            && session.qr.table_order() == TableOrder::Join
            && !holds_for_staff;
        table_id.filter(|_| joins).and_then(|table_id| {
            self.lock_projection()
                .joinable_order_at(table_id, None, session)
        })
    }

    /// The idempotency record a caller's `(sales_channel, external_reference)` already produced at
    /// this store, or `None` ([ADR-0064](../../../../../docs/adr/0064-edge-order-in.md)). The intake path
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
}

/// Who and when every event of one inbound order is stamped with: the box itself, at one instant,
/// on one trading day.
#[derive(Clone, Copy)]
struct SystemStamp {
    device_id: DeviceId,
    now: Timestamp,
    business_date: BusinessDate,
}

/// The box's stamp at `now`, on the trading day `now` falls in for the store's timezone and cutoff.
fn system_stamp(
    device_id: DeviceId,
    now: Timestamp,
    session: &EdgeSession,
) -> Result<SystemStamp, AppError> {
    let business_date = derive_business_date(now, &session.timezone, session.cutoff)
        .map_err(|_ignored| AppError::Clock)?;
    Ok(SystemStamp {
        device_id,
        now,
        business_date,
    })
}

/// An inbound order's events, prepared for its transaction.
struct PreparedOrder {
    /// Each event with the frame that announces it once it commits, in the order written.
    events: Vec<(EventEnvelope<RawPayload>, ServerMessage)>,
    /// Each line's projection record, built from its event.
    records: Vec<(OrderLineId, LineRecord)>,
    /// The notes to hold once the order commits.
    notes: Vec<(OrderLineId, NoteText)>,
}

/// The `sales.order_line.added` for one already-priced line.
fn line_added(
    order_id: OrderId,
    order_line_id: OrderLineId,
    priced: &PricedLine,
    note_present: bool,
) -> SalesOrderLineAdded {
    SalesOrderLineAdded {
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
        modifier_menu_item_ids: priced.modifier_menu_item_ids.clone(),
        // From the same book that priced the line (ADR-0144), not from this function's
        // session snapshot, which the config pull may have swapped since.
        modifier_display_names: priced.modifier_display_names.clone(),
        note_present,
    }
}
