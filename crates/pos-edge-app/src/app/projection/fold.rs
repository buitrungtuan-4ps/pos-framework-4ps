// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! Rebuilding the projection from the event log at boot, folding each event into it, and how far
//! back retention may delete what the projection no longer needs.

use std::num::NonZeroU32;

use pos_core::billing;
use pos_ports::event_store::{EventQuery, EventStore};
use pos_proto::envelope::{EventEnvelope, RawPayload};
use pos_proto::events::{
    BillingBillMerged, BillingBillOpened, BillingBillSettled, BillingBillSplit, BillingBillVoided,
    BillingDiscountApplied, BillingFeeWaived, BillingPaymentCaptured, BillingReceiptReprinted,
    CashDrawerPaidIn, CashDrawerPaidOut, CashShiftClosed, CashShiftCounted, CashShiftOpened,
    EventType, InventoryItemRestored, InventoryItemSoldOut, KitchenTicketBumped, SalesOrderClosed,
    SalesOrderConfirmedByStaff, SalesOrderLineAdded, SalesOrderLineFired, SalesOrderLineUpdated,
    SalesOrderLineVoided, SalesOrderOpened, SalesOrderRejectedByStaff, SalesTableClosed,
    SalesTableMerged, SalesTableOpened, SalesTableTransferred,
};
use pos_proto::time::Timestamp;
use pos_proto::{BillState, OrderLineState, PaymentMethod, ShiftState, TableState};

use crate::app::{
    AppError, BillRecord, CashMovement, Edge, LineRecord, Projection, ShiftRecord, StaffDecision,
};

impl<S: EventStore> Edge<S> {
    /// The instant before which retention may delete a synced event
    /// ([ADR-0145](../../../../../docs/adr/0145-the-edge-keeps-events-until-synced-and-n-days-old.md)):
    /// the store's [`EdgeSession::event_log_days`](crate::app::EdgeSession::event_log_days) ago, or a day before the oldest business still
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
    /// ([ADR-0128](../../../../../docs/adr/0128-a-bill-splits-and-merges.md)), with what each does to the
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
            // And its share of the source's discount and comp (`pos-spec.md` §14.3).
            projection
                .inherit_reductions(event.source_bill_id, &event.resulting_bill_ids)
                .map_err(AppError::Domain)?;
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
        // And what was taken off each, as the merge took it.
        projection
            .absorb_reductions(event.target_bill_id, &event.merged_bill_ids)
            .map_err(AppError::Domain)?;
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

    /// Folds one logged event back into the projection during [`Self::rebuild`].
    ///
    /// The reverse of what each command emits, without re-deciding: the events already happened, so
    /// this trusts them. Events the projection does not track — and unknown types from a newer
    /// writer — are skipped, which is what keeps a forward-compatible log replayable.
    fn fold(
        projection: &mut Projection,
        envelope: &EventEnvelope<RawPayload>,
    ) -> Result<(), AppError> {
        let Some(known) = envelope.event_type.known() else {
            return Ok(());
        };
        match known {
            EventType::SalesTableOpened
            | EventType::SalesTableClosed
            | EventType::SalesTableMerged => Self::fold_table(projection, envelope, known)?,
            EventType::SalesOrderOpened => Self::fold_order_opened(projection, envelope)?,
            EventType::SalesOrderConfirmedByStaff | EventType::SalesOrderRejectedByStaff => {
                Self::fold_staff_decision(projection, envelope, known)?;
            }
            EventType::SalesOrderClosed => Self::fold_order_closed(projection, envelope)?,
            EventType::SalesTableTransferred => Self::fold_transfer(projection, envelope)?,
            EventType::SalesOrderLineAdded
            | EventType::SalesOrderLineFired
            | EventType::SalesOrderLineUpdated => Self::fold_line(projection, envelope, known)?,
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
            EventType::CashShiftOpened
            | EventType::CashShiftCounted
            | EventType::CashShiftClosed => Self::fold_shift(projection, envelope, known)?,
            EventType::CashDrawerPaidIn | EventType::CashDrawerPaidOut => {
                Self::fold_movement(projection, envelope, known)?;
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

    /// Replays a table seated, cleared, or merged into another: the tables a waiter works.
    fn fold_table(
        projection: &mut Projection,
        envelope: &EventEnvelope<RawPayload>,
        known: EventType,
    ) -> Result<(), AppError> {
        match known {
            EventType::SalesTableOpened => {
                let event: SalesTableOpened = envelope.data.decode().map_err(AppError::Encode)?;
                projection.set_table(event.table_id, TableState::Occupied);
                projection.open_order(event.table_id, event.order_id);
            }
            EventType::SalesTableClosed => {
                let event: SalesTableClosed = envelope.data.decode().map_err(AppError::Encode)?;
                projection.set_table(event.table_id, TableState::Free);
            }
            EventType::SalesTableMerged => {
                let event: SalesTableMerged = envelope.data.decode().map_err(AppError::Encode)?;
                projection.merge_order(event.target_order_id, event.merged_order_id);
            }
            // Unreachable: `fold` delegates only the arms above.
            _ => {}
        }
        Ok(())
    }

    /// Replays a line added to an order, fired to the kitchen, or changed in quantity.
    fn fold_line(
        projection: &mut Projection,
        envelope: &EventEnvelope<RawPayload>,
        known: EventType,
    ) -> Result<(), AppError> {
        match known {
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
            // Unreachable: `fold` delegates only the arms above.
            _ => {}
        }
        Ok(())
    }

    /// Replays a cash shift opened, counted, or closed.
    fn fold_shift(
        projection: &mut Projection,
        envelope: &EventEnvelope<RawPayload>,
        known: EventType,
    ) -> Result<(), AppError> {
        match known {
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
            // Unreachable: `fold` delegates only the arms above.
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
}
