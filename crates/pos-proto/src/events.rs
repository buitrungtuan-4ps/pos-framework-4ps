// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The event catalogue: the single source for chain reporting, reconciliation, ERP
//! posting and webhooks.
//!
//! Names are `domain.resource.action` with the action in the past tense, sharing one
//! taxonomy with permission identifiers (`docs/naming-and-api.md` §5). Events already
//! happened, so they are never named imperatively.
//!
//! # Why there are more than the specification lists
//!
//! `docs/pos-spec.md` §18 declares thirty-eight types. Eleven more are declared here,
//! each because a stated requirement had no event able to carry it — most sharply
//! `security.permission.overridden`, since a manager-PIN override above a discount
//! ceiling is a fraud control and had no auditable record at all.
//!
//! The asymmetry is what makes adding them now correct rather than presumptuous:
//! **adding an event type is additive and free; removing one is forbidden.** So the
//! cost of declaring a type early is near zero, and the cost of discovering a missing
//! one after a thousand stores hold offline data is a protocol version bump. Issue #13
//! records the reasoning per event.

use serde::{Deserialize, Serialize};

use crate::enums::{
    PaymentMethod, PaymentOutcome, ReductionKind, SalesChannel, ShipmentStatus,
    StockLedgerEntryKind,
};
use crate::envelope::{DecodeError, EventEnvelope, EventPayload, RawPayload};
use crate::ids::{
    BillId, CampaignId, ConfigVersionId, CourseId, DeviceId, EmployeeId, IngredientId, MenuItemId,
    OrderId, OrderLineId, PaymentId, QrSessionId, ReasonCodeId, ShiftId, ShipmentId, StationId,
    StockLedgerEntryId, TableId, TaxClassId, VoucherId,
};
use crate::money::{Money, Ratio};
use crate::quantity::Quantity;
use crate::text::{DisplayName, PermissionKey, ReleaseTag};
use crate::time::Timestamp;
use crate::wire_enum::Open;

/// Declares the whole catalogue: the [`EventType`] enumeration, one payload struct per
/// entry, the [`TypedPayload`] union, and the decode step.
///
/// Generating all four from one list is what stops them drifting: a payload cannot
/// declare one event type and be registered under another, and a new entry cannot be
/// added to the enumeration without a payload or vice versa.
macro_rules! event_catalogue {
    (
        $(
            $(#[$meta:meta])*
            $variant:ident => $token:literal, version = $version:literal {
                $(
                    $(#[$field_meta:meta])*
                    $field:ident : $field_type:ty
                ),* $(,)?
            }
        ),* $(,)?
    ) => {
        /// Every event type the framework knows.
        ///
        /// Closed on purpose: an unrecognised token from a newer sender is represented
        /// by `None` from [`EventTypeRef::known`](crate::envelope::EventTypeRef::known)
        /// rather than by a variant here, so forward compatibility lives at the wire
        /// and exhaustiveness survives in the code.
        #[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
        pub enum EventType {
            $(
                $(#[$meta])*
                $variant,
            )*
        }

        impl EventType {
            /// Every type, in declaration order.
            pub const ALL: &'static [Self] = &[$(Self::$variant,)*];

            /// The `domain.resource.action` token.
            #[must_use]
            pub const fn as_str(self) -> &'static str {
                match self { $(Self::$variant => $token,)* }
            }

            /// Parses a token. `None` for anything this build does not know.
            #[must_use]
            pub fn parse(token: &str) -> Option<Self> {
                match token { $($token => Some(Self::$variant),)* _ => None }
            }

            /// The payload contract version currently published for this type.
            #[must_use]
            pub const fn schema_version(self) -> u16 {
                match self { $(Self::$variant => $version,)* }
            }

            /// Every field name on this type's payload.
            #[must_use]
            pub const fn field_names(self) -> &'static [&'static str] {
                match self { $(Self::$variant => &[$(stringify!($field),)*],)* }
            }
        }

        impl core::fmt::Display for EventType {
            fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
                f.write_str(self.as_str())
            }
        }

        $(
            $(#[$meta])*
            #[derive(Clone, PartialEq, Debug, Serialize, Deserialize)]
            #[serde(rename_all = "snake_case")]
            pub struct $variant {
                $(
                    $(#[$field_meta])*
                    pub $field: $field_type,
                )*
            }

            impl EventPayload for $variant {
                const EVENT_TYPE: EventType = EventType::$variant;
                const SCHEMA_VERSION: u16 = $version;
                const FIELD_NAMES: &'static [&'static str] = &[$(stringify!($field),)*];
            }

            // The personal-data barrier, applied field by field at compile time. A
            // `String` here fails to compile with the remedy in the error text; see
            // `crate::pii`.
            const _: () = {
                $( $crate::pii::assert_no_pii::<$field_type>(); )*
            };
        )*

        /// A decoded payload.
        #[derive(Clone, PartialEq, Debug)]
        pub enum TypedPayload {
            $(
                $(#[$meta])*
                $variant($variant),
            )*
            /// From a sender speaking a newer vocabulary.
            ///
            /// Retained verbatim rather than dropped, so a node that does not
            /// understand an event can still store it, forward it, include it in a
            /// checksum, and hand it to a consumer that does.
            Unrecognised {
                /// The token as received.
                event_type: String,
                /// The payload bytes, untouched.
                data: RawPayload,
            },
        }

        impl TypedPayload {
            /// The token this payload belongs to, including for an unrecognised one.
            #[must_use]
            pub fn event_type(&self) -> &str {
                match self {
                    $(Self::$variant(_) => $token,)*
                    Self::Unrecognised { event_type, .. } => event_type,
                }
            }
        }

        impl EventEnvelope<RawPayload> {
            /// Decodes the payload according to the envelope's event type.
            ///
            /// An unrecognised type is **not** an error: it yields
            /// [`TypedPayload::Unrecognised`] with the bytes intact.
            ///
            /// # Errors
            ///
            /// [`DecodeError`] only when the type *is* recognised and its payload does
            /// not match the declared shape.
            pub fn into_typed(self) -> Result<EventEnvelope<TypedPayload>, DecodeError> {
                let decoded = match self.event_type.known() {
                    $(
                        Some(EventType::$variant) => {
                            TypedPayload::$variant(self.data.decode()?)
                        }
                    )*
                    None => TypedPayload::Unrecognised {
                        event_type: self.event_type.as_str().to_owned(),
                        data: self.data.clone(),
                    },
                };
                Ok(self.map_data(|_| decoded))
            }
        }
    };
}

event_catalogue! {
    // ---------------------------------------------------------------- sales
    /// An order was opened, on a table or as a takeaway.
    SalesOrderOpened => "sales.order.opened", version = 1 {
        /// The order.
        order_id: OrderId,
        /// Where it came from. Determines the price list and, with the tax class, the
        /// rate.
        channel: Open<SalesChannel>,
        /// The table, for dine-in. Absent for takeaway and marketplace orders.
        table_id: Option<TableId>,
        /// Guest count, for reporting. Optional because staff may not record it.
        guest_count: Option<u16>,
    },
    /// An order reached a terminal state.
    ///
    /// Declared beyond the specification's list: the order lifecycle had no terminal
    /// event distinct from bill settlement, and an order can also end by being voided.
    SalesOrderClosed => "sales.order.closed", version = 1 {
        /// The order.
        order_id: OrderId,
    },
    /// A line was added to an order.
    ///
    /// Carries the **line snapshot** required by `docs/pos-spec.md` §14.2: price, tax
    /// class and the rate in force, and the display name, all captured now. A line
    /// never references the live menu, so editing or deleting a menu item cannot alter
    /// an open order or a settled bill.
    SalesOrderLineAdded => "sales.order_line.added", version = 1 {
        /// The order.
        order_id: OrderId,
        /// The line.
        order_line_id: OrderLineId,
        /// The menu item, for reporting. The amounts below do not depend on it.
        menu_item_id: MenuItemId,
        /// The name shown to the guest at this moment.
        display_name: DisplayName,
        /// How many, in thousandths, so a weighed item or a half can be expressed.
        quantity: Quantity,
        /// Unit price at this moment.
        unit_price: Money,
        /// Extended total for the line at this moment.
        line_total: Money,
        /// Tax class at this moment.
        tax_class_id: TaxClassId,
        /// The tax rate in force at this moment, captured rather than looked up later.
        tax_rate: Ratio,
        /// Seat, when seats are enabled, so a bill can be split by seat.
        seat: Option<u16>,
        /// Course, when courses are enabled.
        course_id: Option<CourseId>,
        /// Whether a guest note was written.
        ///
        /// The note's **text** deliberately never enters the log — see `crate::text`.
        /// A note is where a name and a health condition get typed, and an immutable
        /// log could never erase them.
        note_present: bool,
    },
    /// A line's quantity or total changed while still editable.
    SalesOrderLineUpdated => "sales.order_line.updated", version = 1 {
        /// The order.
        order_id: OrderId,
        /// The line.
        order_line_id: OrderLineId,
        /// New quantity.
        quantity: Quantity,
        /// New extended total.
        line_total: Money,
    },
    /// A line was cancelled.
    SalesOrderLineVoided => "sales.order_line.voided", version = 1 {
        /// The order.
        order_id: OrderId,
        /// The line.
        order_line_id: OrderLineId,
        /// The mandatory reason, from the cloud-managed list.
        reason_code_id: ReasonCodeId,
        /// Whether the kitchen had already started.
        ///
        /// Decides whether this recorded waste rather than returning stock, and whether
        /// a void ticket printed at the station.
        was_fired: bool,
    },
    /// A line was sent to the kitchen. Stock is consumed at this point, not at payment.
    SalesOrderLineFired => "sales.order_line.fired", version = 1 {
        /// The order.
        order_id: OrderId,
        /// The line.
        order_line_id: OrderLineId,
        /// Which station is cooking it.
        station_id: StationId,
        /// When it was fired, for the kitchen display's age timer.
        fire_time: Timestamp,
    },
    /// A line was deliberately withheld from the kitchen.
    ///
    /// Declared beyond the specification's list: §3 requires hold and nothing carried
    /// it.
    SalesOrderLineHeld => "sales.order_line.held", version = 1 {
        /// The order.
        order_id: OrderId,
        /// The line.
        order_line_id: OrderLineId,
    },
    /// A table became occupied.
    ///
    /// Declared beyond the specification's list: the table state machine cycles through
    /// five states and only two transitions had events.
    SalesTableOpened => "sales.table.opened", version = 1 {
        /// The table.
        table_id: TableId,
        /// The order it now holds. A table holds exactly one open order at a time.
        order_id: OrderId,
    },
    /// A table was cleaned and released.
    ///
    /// Declared beyond the specification's list; see [`SalesTableOpened`].
    SalesTableClosed => "sales.table.closed", version = 1 {
        /// The table.
        table_id: TableId,
    },
    /// An order moved to another table.
    SalesTableTransferred => "sales.table.transferred", version = 1 {
        /// The order that moved.
        order_id: OrderId,
        /// Where it was.
        from_table_id: TableId,
        /// Where it is now.
        to_table_id: TableId,
    },
    /// Two orders were combined.
    ///
    /// Each line keeps its origin, so the kitchen is not confused about what was
    /// ordered where.
    SalesTableMerged => "sales.table.merged", version = 1 {
        /// The surviving order.
        target_order_id: OrderId,
        /// The order folded into it.
        merged_order_id: OrderId,
    },
    /// A guest scanned a table's QR code.
    SalesQrSessionStarted => "sales.qr_session.started", version = 1 {
        /// The session.
        qr_session_id: QrSessionId,
        /// The table, from the signed code.
        table_id: TableId,
    },
    /// A guest submitted an order from the QR web application.
    SalesOrderSubmittedByGuest => "sales.order.submitted_by_guest", version = 1 {
        /// The order the lines will join or create.
        order_id: OrderId,
        /// The session it came from.
        qr_session_id: QrSessionId,
        /// How many lines, for the per-table rate limit and the throttle window.
        line_count: u16,
    },
    /// Staff confirmed a guest's submission, which is required before firing by default.
    SalesOrderConfirmedByStaff => "sales.order.confirmed_by_staff", version = 1 {
        /// The order.
        order_id: OrderId,
    },
    /// Staff rejected a guest's submission.
    SalesOrderRejectedByStaff => "sales.order.rejected_by_staff", version = 1 {
        /// The order.
        order_id: OrderId,
        /// Why.
        reason_code_id: ReasonCodeId,
    },

    // -------------------------------------------------------------- kitchen
    /// A station marked its ticket complete.
    KitchenTicketBumped => "kitchen.ticket.bumped", version = 1 {
        /// The order.
        order_id: OrderId,
        /// The station.
        station_id: StationId,
        /// Which lines were bumped.
        order_line_ids: Vec<OrderLineId>,
    },
    /// A bumped ticket was brought back, within the sixty-second window.
    KitchenTicketRecalled => "kitchen.ticket.recalled", version = 1 {
        /// The order.
        order_id: OrderId,
        /// The station.
        station_id: StationId,
    },

    // -------------------------------------------------------------- billing
    /// A bill was created for an order.
    ///
    /// Declared beyond the specification's list: split, merged, settled and voided all
    /// presupposed a bill that nothing created.
    BillingBillOpened => "billing.bill.opened", version = 1 {
        /// The bill.
        bill_id: BillId,
        /// The order it bills.
        order_id: OrderId,
    },
    /// A bill was split.
    ///
    /// The parts sum exactly to the original, because splitting partitions amounts that
    /// were already allocated and snapshotted rather than recomputing percentages.
    BillingBillSplit => "billing.bill.split", version = 1 {
        /// The bill that was split.
        source_bill_id: BillId,
        /// The bills produced.
        resulting_bill_ids: Vec<BillId>,
    },
    /// Bills were combined before payment.
    BillingBillMerged => "billing.bill.merged", version = 1 {
        /// The surviving bill.
        target_bill_id: BillId,
        /// The bills folded into it.
        merged_bill_ids: Vec<BillId>,
    },
    /// A bill was settled. A one-time transition; a second attempt is refused.
    BillingBillSettled => "billing.bill.settled", version = 1 {
        /// The bill.
        bill_id: BillId,
        /// The gapless per-store receipt number, allocated in the same transaction as
        /// the bill.
        ///
        /// Distinct from a legal invoice number, which the country module issues from a
        /// pre-allocated range. Conflating the two is forbidden.
        receipt_number: u64,
        /// Sum of line totals before any reduction.
        subtotal: Money,
        /// Total discounts and comps applied.
        reduction_total: Money,
        /// Service charge, applied after reductions and before tax.
        service_charge: Money,
        /// Tax, summed across tax classes.
        tax_total: Money,
        /// The explicit cash-rounding line.
        ///
        /// Materialised rather than applied silently, which is what lets the payment
        /// total reconcile exactly and the printed lines add up to the printed total.
        rounding_adjustment: Money,
        /// What the guest owes: subtotal − reductions + service charge + tax + rounding.
        total_due: Money,
    },
    /// A settled bill was voided, which requires a manager and a reason.
    BillingBillVoided => "billing.bill.voided", version = 1 {
        /// The bill.
        bill_id: BillId,
        /// Why.
        reason_code_id: ReasonCodeId,
    },
    /// A price reduction was applied.
    BillingDiscountApplied => "billing.discount.applied", version = 1 {
        /// The bill.
        bill_id: BillId,
        /// The line, for a line-level reduction. Absent for a bill-level one.
        order_line_id: Option<OrderLineId>,
        /// The campaign, when a rule granted it rather than a person.
        campaign_id: Option<CampaignId>,
        /// Discount, comp, or void — three concepts accounting treats differently.
        kind: Open<ReductionKind>,
        /// How much came off.
        amount: Money,
        /// The mandatory reason for a manual reduction.
        reason_code_id: Option<ReasonCodeId>,
    },
    /// An item was given away.
    ///
    /// Distinct from a discount because a comp **still consumes inventory** and is
    /// recorded as cost, and distinct from a void because the item did happen.
    BillingCompApplied => "billing.comp.applied", version = 1 {
        /// The bill.
        bill_id: BillId,
        /// The line given away.
        order_line_id: OrderLineId,
        /// The value forgone, which is recorded as cost rather than lost revenue.
        amount: Money,
        /// Why.
        reason_code_id: ReasonCodeId,
    },
    /// A payment was taken.
    BillingPaymentCaptured => "billing.payment.captured", version = 1 {
        /// The bill.
        bill_id: BillId,
        /// The payment.
        payment_id: PaymentId,
        /// How it was paid.
        method: Open<PaymentMethod>,
        /// What the terminal concluded.
        ///
        /// `PAYMENT_OUTCOME_UNKNOWN` is a real outcome, not an absent one: the bill
        /// parks for reconciliation rather than the system guessing.
        outcome: Open<PaymentOutcome>,
        /// What the guest handed over.
        tendered: Money,
        /// How much of it settled the bill.
        ///
        /// Separate from `tendered` because they differ whenever there is change or a
        /// tip, which is why "payments sum to the bill total" is only true of this
        /// field.
        applied_to_bill: Money,
        /// Change returned.
        change_given: Money,
        /// Tip, held apart from the sale amount and outside `total_due`.
        tip_amount: Money,
    },
    /// A tip was adjusted after the card was captured.
    BillingTipAdjusted => "billing.tip.adjusted", version = 1 {
        /// The payment.
        payment_id: PaymentId,
        /// The tip after adjustment.
        tip_amount: Money,
    },
    /// Money was returned. Only at the store that issued the bill.
    BillingRefundIssued => "billing.refund.issued", version = 1 {
        /// The bill.
        bill_id: BillId,
        /// How much.
        amount: Money,
        /// Why.
        reason_code_id: ReasonCodeId,
    },

    // ----------------------------------------------------------------- cash
    /// A shift opened with a starting float.
    CashShiftOpened => "cash.shift.opened", version = 1 {
        /// The shift.
        opened_shift_id: ShiftId,
        /// Cash in the drawer at the start.
        opening_float: Money,
    },
    /// The cashier entered a counted amount.
    ///
    /// Declared beyond the specification's list, and it matters: the close is **blind**,
    /// so the count is recorded *before* the expected amount is revealed. Folding it
    /// into the close would make the blindness unverifiable afterwards, which defeats
    /// the control.
    CashShiftCounted => "cash.shift.counted", version = 1 {
        /// The shift.
        counted_shift_id: ShiftId,
        /// What the cashier counted, entered before seeing what was expected.
        counted_amount: Money,
        /// When it was counted.
        count_time: Timestamp,
    },
    /// A shift closed and locked.
    CashShiftClosed => "cash.shift.closed", version = 1 {
        /// The shift.
        closed_shift_id: ShiftId,
        /// What the system expected.
        expected_amount: Money,
        /// What was counted.
        counted_amount: Money,
        /// Counted minus expected. Negative means short.
        variance: Money,
    },
    /// The drawer opened.
    CashDrawerOpened => "cash.drawer.opened", version = 1 {
        /// Whether it opened outside a sale, which needs a permission and a reason.
        standalone: bool,
        /// Why, for a standalone opening.
        reason_code_id: Option<ReasonCodeId>,
    },
    /// Cash was added to the drawer.
    CashDrawerPaidIn => "cash.drawer.paid_in", version = 1 {
        /// How much.
        amount: Money,
        /// Why.
        reason_code_id: ReasonCodeId,
    },
    /// Cash was removed from the drawer.
    CashDrawerPaidOut => "cash.drawer.paid_out", version = 1 {
        /// How much.
        amount: Money,
        /// Why.
        reason_code_id: ReasonCodeId,
    },

    // ------------------------------------------------------------ inventory
    /// An item became unavailable, greying out on every device.
    InventoryItemSoldOut => "inventory.item.sold_out", version = 1 {
        /// The item.
        menu_item_id: MenuItemId,
        /// Whether an ingredient threshold triggered it rather than a person.
        automatic: bool,
    },
    /// An item became available again.
    InventoryItemRestored => "inventory.item.restored", version = 1 {
        /// The item.
        menu_item_id: MenuItemId,
    },
    /// Firing a line consumed stock.
    InventoryStockConsumed => "inventory.stock.consumed", version = 1 {
        /// The ledger entry.
        stock_ledger_entry_id: StockLedgerEntryId,
        /// What was consumed.
        ingredient_id: IngredientId,
        /// How much.
        quantity: Quantity,
        /// The line that caused it.
        order_line_id: OrderLineId,
        /// Why this entry exists.
        kind: Open<StockLedgerEntryKind>,
    },
    /// Goods were received.
    ///
    /// Declared beyond the specification's list: §8 names receipt as one of the five
    /// ledger entry kinds and nothing carried it.
    InventoryStockReceived => "inventory.stock.received", version = 1 {
        /// The ledger entry.
        stock_ledger_entry_id: StockLedgerEntryId,
        /// What arrived.
        ingredient_id: IngredientId,
        /// How much.
        quantity: Quantity,
        /// Why this entry exists.
        kind: Open<StockLedgerEntryKind>,
    },
    /// Stock was corrected by hand.
    InventoryStockAdjusted => "inventory.stock.adjusted", version = 1 {
        /// The ledger entry.
        stock_ledger_entry_id: StockLedgerEntryId,
        /// What was adjusted.
        ingredient_id: IngredientId,
        /// The signed change.
        quantity: Quantity,
        /// Why.
        reason_code_id: ReasonCodeId,
        /// Why this entry exists.
        kind: Open<StockLedgerEntryKind>,
    },
    /// Stock was written off.
    ///
    /// Declared beyond the specification's list: §8 says cancelling a fired line records
    /// **waste** and does not return stock, and the ledger has waste entries while the
    /// catalogue had no waste event.
    InventoryStockWasted => "inventory.stock.wasted", version = 1 {
        /// The ledger entry.
        stock_ledger_entry_id: StockLedgerEntryId,
        /// What was wasted.
        ingredient_id: IngredientId,
        /// How much.
        quantity: Quantity,
        /// The line, when a void after firing caused it.
        order_line_id: Option<OrderLineId>,
        /// Why.
        reason_code_id: ReasonCodeId,
        /// Why this entry exists.
        kind: Open<StockLedgerEntryKind>,
    },
    /// A physical count was recorded.
    InventoryStockCounted => "inventory.stock.counted", version = 1 {
        /// The ledger entry.
        stock_ledger_entry_id: StockLedgerEntryId,
        /// What was counted.
        ingredient_id: IngredientId,
        /// What the counter found.
        counted_qty: Quantity,
        /// When, so the delta can be computed against the projection **at that moment**
        /// and sales during the count do not corrupt it.
        count_time: Timestamp,
        /// Counted minus projected, as at `count_time`.
        delta: Quantity,
        /// Why this entry exists.
        kind: Open<StockLedgerEntryKind>,
    },

    // ------------------------------------------------------------ promotion
    /// A voucher was reserved against a bill.
    ///
    /// Declared beyond the specification's list, and necessary: redemption is an atomic
    /// check-and-mark, so reserve and redeem are distinct states. With only a redemption
    /// event, a settlement that then failed would burn the voucher.
    PromotionVoucherReserved => "promotion.voucher.reserved", version = 1 {
        /// The voucher.
        voucher_id: VoucherId,
        /// The bill it is held against.
        bill_id: BillId,
    },
    /// A voucher was spent.
    PromotionVoucherRedeemed => "promotion.voucher.redeemed", version = 1 {
        /// The voucher.
        voucher_id: VoucherId,
        /// The bill.
        bill_id: BillId,
        /// The campaign it belongs to.
        campaign_id: CampaignId,
        /// How much it was worth.
        amount: Money,
    },

    // ------------------------------------------------------------- delivery
    /// A courier job was created.
    DeliveryShipmentCreated => "delivery.shipment.created", version = 1 {
        /// The shipment.
        shipment_id: ShipmentId,
        /// The order being delivered.
        order_id: OrderId,
    },
    /// A courier reported progress.
    DeliveryShipmentStatusChanged => "delivery.shipment.status_changed", version = 1 {
        /// The shipment.
        shipment_id: ShipmentId,
        /// Where it has got to.
        status: Open<ShipmentStatus>,
    },

    // ------------------------------------------------------------- security
    /// A permission ceiling was overridden by a manager.
    ///
    /// Declared beyond the specification's list, and the most consequential of the
    /// additions. A manager-PIN override above a discount ceiling is one of the
    /// framework's named fraud controls, and no existing event could carry it — so the
    /// control had no auditable record at all.
    SecurityPermissionOverridden => "security.permission.overridden", version = 1 {
        /// Which permission was exercised.
        permission_key: PermissionKey,
        /// Who authorised it. The employee who *requested* it is on the envelope.
        approver_employee_id: EmployeeId,
        /// The amount by which the actor's ceiling was exceeded, where there is one.
        exceeded_by: Option<Money>,
    },

    // ---------------------------------------------------- config and fleet
    /// A configuration version was published in the cloud.
    ConfigVersionPublished => "config.version.published", version = 1 {
        /// The version.
        config_version_id: ConfigVersionId,
        /// Its monotonic sequence number.
        version: u64,
    },
    /// A device started running a configuration version.
    ///
    /// Declared beyond the specification's list: published is not the same as active,
    /// and the fleet view needs to know which store is actually running what.
    ConfigVersionActivated => "config.version.activated", version = 1 {
        /// The version.
        config_version_id: ConfigVersionId,
        /// Its monotonic sequence number.
        version: u64,
    },
    /// A device finished activation and became able to trade.
    DeviceActivationCompleted => "device.activation.completed", version = 1 {
        /// The device that was activated, which may differ from the reporting device.
        activated_device_id: DeviceId,
    },
    /// A release reached a store.
    FleetUpdateRolledOut => "fleet.update.rolled_out", version = 1 {
        /// The release.
        release_tag: ReleaseTag,
        /// Which ring the store is in.
        ring: u8,
    },
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use super::{EventType, SalesOrderOpened, TypedPayload};
    use crate::enums::SalesChannel;
    use crate::envelope::{EventEnvelope, EventPayload, EventTypeRef, RawPayload};
    use crate::ids::{BrandId, DeviceId, EventId, OrderId, StoreId, TableId, TenantId};
    use crate::pii::check_field_name;
    use crate::time::{BusinessDate, Timestamp};
    use crate::ulid::Ulid;
    use crate::wire_enum::Open;

    /// The thirty-eight types `docs/pos-spec.md` §18 declares.
    ///
    /// Held here as a literal list so that dropping one is a test failure rather than a
    /// silent protocol break. A published event name is a contract: it may be added to,
    /// never renamed or removed.
    const SPECIFIED: &[&str] = &[
        "sales.order.opened",
        "sales.order_line.added",
        "sales.order_line.updated",
        "sales.order_line.voided",
        "sales.order_line.fired",
        "sales.table.transferred",
        "sales.table.merged",
        "sales.qr_session.started",
        "sales.order.submitted_by_guest",
        "sales.order.confirmed_by_staff",
        "sales.order.rejected_by_staff",
        "kitchen.ticket.bumped",
        "kitchen.ticket.recalled",
        "billing.bill.split",
        "billing.bill.merged",
        "billing.bill.settled",
        "billing.bill.voided",
        "billing.discount.applied",
        "billing.comp.applied",
        "billing.payment.captured",
        "billing.tip.adjusted",
        "billing.refund.issued",
        "cash.shift.opened",
        "cash.shift.closed",
        "cash.drawer.opened",
        "cash.drawer.paid_in",
        "cash.drawer.paid_out",
        "inventory.item.sold_out",
        "inventory.item.restored",
        "inventory.stock.consumed",
        "inventory.stock.adjusted",
        "inventory.stock.counted",
        "promotion.voucher.redeemed",
        "delivery.shipment.created",
        "delivery.shipment.status_changed",
        "config.version.published",
        "device.activation.completed",
        "fleet.update.rolled_out",
    ];

    fn sample_envelope<D>(event_type: EventType, data: D) -> EventEnvelope<D> {
        EventEnvelope {
            event_id: EventId::new(Ulid::from_parts(1_767_225_600_000, 1)),
            event_type: EventTypeRef::from_known(event_type),
            event_time: Timestamp::from_milliseconds_since_epoch(1_767_225_600_000)
                .expect("instant"),
            business_date: BusinessDate::from_ymd(2026, 8, 13).expect("date"),
            schema_version: event_type.schema_version(),
            tenant_id: TenantId::new(Ulid::from_parts(1, 1)),
            brand_id: BrandId::new(Ulid::from_parts(1, 2)),
            store_id: StoreId::new(Ulid::from_parts(1, 3)),
            device_id: DeviceId::new(Ulid::from_parts(1, 4)),
            employee_id: None,
            shift_id: None,
            data,
        }
    }

    #[test]
    fn every_specified_event_type_is_present() {
        let declared: BTreeSet<&str> = EventType::ALL.iter().map(|t| t.as_str()).collect();
        for specified in SPECIFIED {
            assert!(
                declared.contains(specified),
                "{specified} is in pos-spec.md §18 but not in the catalogue — a \
                 published event name may never be removed"
            );
        }
        assert_eq!(SPECIFIED.len(), 38, "the specification declares 38 types");
    }

    #[test]
    fn the_catalogue_extends_the_specification_rather_than_replacing_it() {
        let specified: BTreeSet<&str> = SPECIFIED.iter().copied().collect();
        let extra: Vec<&str> = EventType::ALL
            .iter()
            .map(|t| t.as_str())
            .filter(|token| !specified.contains(token))
            .collect();
        // Each addition is justified in issue #13 and in this module's documentation.
        assert_eq!(
            extra.len(),
            EventType::ALL.len() - 38,
            "every type is either specified or a documented addition"
        );
        assert!(
            extra.contains(&"security.permission.overridden"),
            "the manager-override audit event is the most important addition"
        );
    }

    #[test]
    fn every_token_is_unique() {
        let unique: BTreeSet<&str> = EventType::ALL.iter().map(|t| t.as_str()).collect();
        assert_eq!(
            unique.len(),
            EventType::ALL.len(),
            "two event types share a token"
        );
    }

    #[test]
    fn every_token_follows_domain_resource_action() {
        for event_type in EventType::ALL {
            let token = event_type.as_str();
            let segments: Vec<&str> = token.split('.').collect();
            assert_eq!(segments.len(), 3, "{token} is not domain.resource.action");
            for segment in &segments {
                assert!(!segment.is_empty(), "{token} has an empty segment");
                assert!(
                    segment
                        .bytes()
                        .all(|b| b.is_ascii_lowercase() || b == b'_' || b.is_ascii_digit()),
                    "{token} is not snake_case throughout"
                );
            }
        }
    }

    #[test]
    fn every_token_round_trips() {
        for event_type in EventType::ALL {
            assert_eq!(
                EventType::parse(event_type.as_str()),
                Some(*event_type),
                "{event_type} does not parse back to itself"
            );
        }
    }

    #[test]
    fn no_payload_field_name_suggests_personal_data() {
        // The second layer of the personal-data barrier: the type system cannot tell a
        // translation key from a guest's name, so the field names are checked too.
        for event_type in EventType::ALL {
            for field in event_type.field_names() {
                assert_eq!(
                    check_field_name(field),
                    Ok(()),
                    "{event_type} has field `{field}`, which suggests personal data — \
                     carry a subject_id instead"
                );
            }
        }
    }

    #[test]
    fn every_payload_field_name_is_snake_case() {
        // Which is also what makes the wire name equal the Rust name equal the database
        // column name, so no mapping layer exists anywhere.
        for event_type in EventType::ALL {
            for field in event_type.field_names() {
                assert!(
                    field
                        .bytes()
                        .all(|b| b.is_ascii_lowercase() || b == b'_' || b.is_ascii_digit()),
                    "{event_type}.{field} is not snake_case"
                );
                assert_ne!(*field, "id", "{event_type} has a bare `id` field");
                assert!(
                    !field.ends_with("_at"),
                    "{event_type}.{field} ends in _at; timestamps end in _time"
                );
            }
        }
    }

    #[test]
    fn every_payload_declares_a_positive_schema_version() {
        for event_type in EventType::ALL {
            assert!(
                event_type.schema_version() >= 1,
                "{event_type} has schema version 0, which collides with 'absent'"
            );
        }
    }

    #[test]
    fn an_envelope_round_trips_through_json() {
        let payload = SalesOrderOpened {
            order_id: OrderId::new(Ulid::from_parts(1_767_225_600_000, 9)),
            channel: Open::from_known(SalesChannel::DineIn),
            table_id: Some(TableId::new(Ulid::from_parts(1, 12))),
            guest_count: Some(4),
        };
        let envelope = sample_envelope(EventType::SalesOrderOpened, payload.clone());
        let json = serde_json::to_string(&envelope).expect("serialise");

        // The documented envelope field names, verbatim.
        for field in [
            "event_id",
            "event_type",
            "event_time",
            "business_date",
            "schema_version",
            "tenant_id",
            "brand_id",
            "store_id",
            "device_id",
            "data",
        ] {
            assert!(json.contains(&format!("\"{field}\"")), "{field} is missing");
        }

        let back: EventEnvelope<SalesOrderOpened> =
            serde_json::from_str(&json).expect("deserialise");
        assert_eq!(back.data, payload);
        assert_eq!(back.event_type.as_str(), "sales.order.opened");
        assert_eq!(back.business_date.to_string(), "2026-08-13");
    }

    #[test]
    fn business_date_is_carried_and_is_not_derived_from_the_event_time() {
        // `pos-spec.md` §14.1: a bill rung at 01:30 belongs to the previous evening, so
        // the business date needs the store's timezone and cut-off hour and cannot be
        // recovered from the instant alone. The archive's envelope omitted the field
        // entirely; the English set is right to carry it.
        //
        // The fixture deliberately sets a business date in August against an instant in
        // January. Nothing in the envelope reconciles them, and nothing should: the
        // device computed the business date at capture, and recomputing it downstream
        // would rewrite history the next time a store's cut-off was edited.
        let envelope = sample_envelope(EventType::SalesOrderOpened, ());
        let json = serde_json::to_string(&envelope).expect("serialise");

        assert!(
            json.contains(r#""business_date":"2026-08-13""#),
            "got {json}"
        );
        assert_eq!(
            envelope.event_time.to_string(),
            "2026-01-01T00:00:00Z",
            "the instant is unrelated to the business date"
        );
        assert!(
            json.contains(&format!(r#""event_time":"{}""#, envelope.event_time)),
            "got {json}"
        );
    }

    #[test]
    fn an_absent_employee_is_omitted_rather_than_null() {
        // System-originated events — a published config version, a fleet rollout — have
        // no employee. The envelope's shape is still identical on every channel.
        let envelope = sample_envelope(EventType::ConfigVersionPublished, ());
        let json = serde_json::to_string(&envelope).expect("serialise");
        assert!(
            !json.contains("employee_id"),
            "expected omission, got {json}"
        );
        assert!(!json.contains("shift_id"));
    }

    #[test]
    fn an_unrecognised_event_type_decodes_losslessly() {
        // The property that lets an older edge store, forward and checksum an event
        // from a newer cloud instead of dropping it.
        let json = r#"{
            "event_id": "01JQ0000000000000000000001",
            "event_type": "sales.order.teleported",
            "event_time": "2026-08-14T03:12:45.123Z",
            "business_date": "2026-08-13",
            "schema_version": 7,
            "tenant_id": "01JQ0000000000000000000002",
            "brand_id": "01JQ0000000000000000000003",
            "store_id": "01JQ0000000000000000000004",
            "device_id": "01JQ0000000000000000000005",
            "data": {"destination": "mars", "crew": 3}
        }"#;
        let received: EventEnvelope<RawPayload> =
            serde_json::from_str(json).expect("the envelope still parses");
        assert!(received.event_type.known().is_none());

        let typed = received
            .into_typed()
            .expect("an unknown type is not an error");
        // A let-else rather than a match: a wildcard arm over forty-nine variants
        // would also silently swallow any type added later, which is the whole
        // reason `clippy::wildcard_enum_match_arm` is denied.
        let TypedPayload::Unrecognised { event_type, data } = typed.data else {
            panic!("expected Unrecognised for an unknown event type");
        };
        assert_eq!(event_type, "sales.order.teleported");
        // Bytes intact, including the fields this build has never heard of.
        assert!(data.as_json().contains("\"destination\""));
        assert!(data.as_json().contains("\"crew\""));
    }

    #[test]
    fn a_recognised_type_with_a_malformed_payload_is_an_error() {
        // Tolerance is for vocabulary, not for corruption. An event whose type we know
        // and whose payload we cannot read is a real fault worth surfacing.
        let json = r#"{
            "event_id": "01JQ0000000000000000000001",
            "event_type": "sales.order.opened",
            "event_time": "2026-08-14T03:12:45.123Z",
            "business_date": "2026-08-13",
            "schema_version": 1,
            "tenant_id": "01JQ0000000000000000000002",
            "brand_id": "01JQ0000000000000000000003",
            "store_id": "01JQ0000000000000000000004",
            "device_id": "01JQ0000000000000000000005",
            "data": {"order_id": "not-a-ulid"}
        }"#;
        let received: EventEnvelope<RawPayload> = serde_json::from_str(json).expect("parses");
        assert!(received.into_typed().is_err());
    }

    #[test]
    fn an_added_payload_field_does_not_break_an_older_reader() {
        // This is what "protocol changes are additive" has to mean in practice: a cloud
        // adds a field to one event's payload, without a PROTOCOL_VERSION bump, and an
        // edge running last month's build still reads it.
        let json = r#"{
            "order_id": "01JQ0000000000000000000009",
            "channel": "SALES_CHANNEL_DINE_IN",
            "table_id": null,
            "guest_count": 4,
            "loyalty_tier_added_next_release": "GOLD"
        }"#;
        let payload: SalesOrderOpened =
            serde_json::from_str(json).expect("an unknown field must not break the read");
        assert_eq!(payload.guest_count, Some(4));
    }

    #[test]
    fn the_payload_trait_agrees_with_the_enumeration() {
        // The macro generates both from one declaration, so this asserts the macro
        // rather than the data — which is the point: they cannot drift.
        assert_eq!(SalesOrderOpened::EVENT_TYPE, EventType::SalesOrderOpened);
        assert_eq!(
            SalesOrderOpened::SCHEMA_VERSION,
            EventType::SalesOrderOpened.schema_version()
        );
        assert_eq!(
            SalesOrderOpened::FIELD_NAMES,
            EventType::SalesOrderOpened.field_names()
        );
    }
}
