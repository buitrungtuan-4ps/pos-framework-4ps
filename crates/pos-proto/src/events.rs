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
//! `docs/pos-spec.md` §18 declares thirty-eight types. Sixteen more are declared here,
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

use crate::chain::ChainHash;
use crate::enums::{
    PaymentMethod, PaymentOutcome, ReductionKind, SalesChannel, ShipmentStatus,
    StockLedgerEntryKind,
};
use crate::envelope::{DecodeError, EventEnvelope, EventPayload, RawPayload};
use crate::fees::{FeeCode, FrozenFee};
use crate::ids::{
    BillId, CampaignId, ConfigVersionId, CourseId, DeviceId, EmployeeId, FeeId, IngredientId,
    MenuItemId, OrderId, OrderLineId, PaymentId, QrSessionId, ReasonCodeId, ShiftId, ShipmentId,
    StationId, StockLedgerEntryId, SubjectId, SupplierId, TableId, TaxClassId, VoucherId,
};
use crate::locale::TaxComponentName;
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
        /// The modifiers chosen for this line, each itself a catalog item with its own
        /// price and its own recipe (`docs/pos-spec.md` §8).
        ///
        /// Captured because **firing consumes them**: a large pizza is the base recipe
        /// plus the "large" modifier's extra dough, and a line that does not record
        /// which modifiers it carries cannot deduct them. Until this field existed the
        /// edge fired every line with an empty modifier list, so every modifier's
        /// ingredients were sold and never taken off the shelf.
        ///
        /// Their *prices* are already inside `unit_price`, which is the summed figure
        /// the guest is charged; these ids are what the kitchen and the stock ledger
        /// need, not a second source of truth for money.
        ///
        /// `#[serde(default)]` because this is an additive field on a published payload:
        /// a store upgrading replays its own log at start-up, and every line written
        /// before this release has no such key. Without the default that replay fails
        /// and the box will not boot.
        #[serde(default)]
        modifier_menu_item_ids: Vec<MenuItemId>,
        /// The names the modifiers had when the line was priced, index-aligned with
        /// `modifier_menu_item_ids`
        /// ([ADR-0144](../../../docs/adr/0144-a-line-records-the-names-of-its-modifiers.md)).
        ///
        /// What the receipt prints under the line: a receipt shows what the guest agreed
        /// to, not today's spelling (ADR-0129). Menu text, not personal data.
        ///
        /// `#[serde(default)]` for the same reason as the ids: a line written before this
        /// field replays with none, and its receipt prints no modifiers rather than names
        /// looked up today.
        #[serde(default)]
        modifier_display_names: Vec<DisplayName>,
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
        /// The order lines this bill covers
        /// ([ADR-0128](../../../docs/adr/0128-a-bill-splits-and-merges.md) decisions 1 and 9).
        ///
        /// A bill covers *lines*, not an order — which is what makes a split a partition of
        /// something. Recorded here rather than only in the projection because otherwise the log
        /// says which bills a split produced and never which lines went into which, so the property
        /// the split event states about itself cannot be checked from the store's own log, and a
        /// store replaying after a restart could not rebuild which bill owed what.
        ///
        /// On `opened` rather than on `split`, because a bill covers lines from the moment it
        /// exists — a bill born by splitting and a bill born by opening a table are the same kind of
        /// thing, and putting the partition on the split event would make the same fact true in two
        /// places. A merge needs no new field: the target's set is its own plus every absorbed
        /// bill's, and both are already in the log.
        ///
        /// `#[serde(default)]` so a bill opened before this field existed still replays — as a bill
        /// covering no lines it names, which the projection reads as "every unvoided line on the
        /// order", exactly what such a bill meant when it was written.
        #[serde(default)]
        order_line_ids: Vec<OrderLineId>,
        /// The fee rules in force for the bill when it opened, frozen
        /// ([ADR-0159](../../../docs/adr/0159-a-fee-is-configuration.md) decision 3): active,
        /// clear of every fault, and applying on the bill's channel. The bill is computed from
        /// these until it settles, so a publish during the meal does not change what its pre-bill
        /// showed.
        ///
        /// Each rule carries what the bill is computed from, its code, and its own name as
        /// published, frozen as a line's is (ADR-0129), and never a translation. Left out when
        /// empty, and absent reads as empty: a bill opened before this field existed, or with no
        /// rule in force, has no fees.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        fee_rules: Vec<FrozenFee>,
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
        /// The buyer this invoice was issued to, for a B2B tax invoice
        /// ([ADR-0107](../../../docs/adr/0107-the-buyer-is-a-subject.md)).
        ///
        /// A Japanese qualified invoice and an Indian tax invoice both have to name their
        /// buyer, and the buyer's name and registration number may not enter this log —
        /// `crate::pii` makes that a compile error, and `docs/pos-spec.md` §15 explains
        /// why: the log is immutable, so anything personal in it could never be erased.
        ///
        /// So the event carries the **identifier** and the details live in the store's
        /// subject store keyed by it, written in the settle's own transaction. Erasing a
        /// buyer is then scrubbing one row, and this event keeps its subtotal, its tax
        /// lines and its total — the figures still reconcile with the person gone.
        ///
        /// `None` on every ordinary retail sale, which is nearly every bill.
        /// `#[serde(default)]` for the same reason `modifier_menu_item_ids` carries one:
        /// a store upgrading replays its own log at start-up, and every bill settled
        /// before this release has no such key.
        #[serde(default)]
        buyer_subject_id: Option<SubjectId>,
        /// One line per fee the bill was charged, in the order of its rules (ADR-0159 decision 4).
        /// `service_charge` keeps carrying their sum, so a reader of that one figure stays right
        /// until it reads these.
        ///
        /// Left out when empty, and absent reads as empty: a bill settled before this field
        /// existed, or charged no fee, has no fee lines.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        fee_lines: Vec<BillFeeLine>,
        /// The bill's tax per class, each rounded once on its class's whole base (ADR-0028), and
        /// summing to `tax_total` (ADR-0159 decision 4, roadmap-v3 B4.1).
        ///
        /// Left out when empty. Absent means *not recorded*, not *no tax*: a bill settled before
        /// this field existed has none, and a reader falls back to `tax_total`, as a reprinted
        /// copy already does (ADR-0164).
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        tax_lines: Vec<BillTaxLine>,
    },
    /// A settled bill was voided, which requires a manager and a reason.
    BillingBillVoided => "billing.bill.voided", version = 1 {
        /// The bill.
        bill_id: BillId,
        /// Why.
        reason_code_id: ReasonCodeId,
    },
    /// A settled bill's receipt was printed again, as a copy marked COPY
    /// ([ADR-0164](../../../docs/adr/0164-a-receipt-is-reprinted-as-a-marked-copy-and-every-reprint-is-counted.md)).
    ///
    /// One event per copy, so a reprint rate is a count: the envelope already names the employee
    /// and the device, which is what `docs/pos-spec.md` §11 item 3 compares against peers. A copy is
    /// printed under the receipt's own number and never takes a new one, so the gapless series
    /// (ADR-0025) counts sales, not pieces of paper. No PII: a buyer's details stay in the subject
    /// store, as they do for the original.
    BillingReceiptReprinted => "billing.receipt.reprinted", version = 1 {
        /// The settled bill whose receipt was printed again.
        bill_id: BillId,
        /// Its receipt number: the one the original printed.
        receipt_number: u64,
        /// Which copy this is, counting from 1 for the first reprint.
        copy_number: u32,
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
    /// A fee was waived on one bill before it settled
    /// ([ADR-0159](../../../docs/adr/0159-a-fee-is-configuration.md) decision 5).
    ///
    /// Declared beyond the specification's list: a waive is an act, not an edit of the rule, so
    /// the fee stays published and every other bill is charged it, and a fee left off one bill with
    /// no event would be money forgiven that nothing records. The bill charges nothing for the fee
    /// from then on and taxes nothing for it, and every part of a split of it and every merge it
    /// joins keeps it waived. Ids only: what the fee came to is the bill's to compute, and who
    /// waived it and who approved it are the envelope's and `security.permission.overridden`'s.
    BillingFeeWaived => "billing.fee.waived", version = 1 {
        /// The bill.
        bill_id: BillId,
        /// The fee waived, a rule the bill froze when it opened.
        fee_id: FeeId,
        /// Why, from the managed list.
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
        /// The till whose drawer this is: the `TERMINAL` entry's `device_id` on the `devices` node
        /// ([ADR-0167](../../../docs/adr/0167-a-till-has-its-own-cash-drawer.md) decision 5), never
        /// the envelope's `device_id`, which is the device that recorded the event. Absent is the
        /// store's drawer and is left off the wire, so a store with one drawer writes the bytes it
        /// wrote before the field existed.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        terminal_device_id: Option<DeviceId>,
        /// The person the drawer is, for this session, assigned to, where the store assigns drawers
        /// ([ADR-0167](../../../docs/adr/0167-a-till-has-its-own-cash-drawer.md) decision 9): an id,
        /// never a name. Absent where drawers are shared, and left off the wire, so a store that
        /// shares them writes the bytes it wrote before the field existed.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        assigned_employee_id: Option<EmployeeId>,
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
        /// The till whose drawer this is, as [`CashShiftOpened::terminal_device_id`] says.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        terminal_device_id: Option<DeviceId>,
        /// Why the drawer is over or short, where the variance is beyond the store's
        /// `shift.variance_reason_minor` and the store's list offers a reason for it
        /// ([ADR-0167](../../../docs/adr/0167-a-till-has-its-own-cash-drawer.md) decision 12,
        /// `REASON_ACTION_CASH_VARIANCE`). Absent otherwise, and left off the wire, so a close that
        /// owes no reason writes the bytes it wrote before the field existed.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reason_code_id: Option<ReasonCodeId>,
    },
    /// The drawer opened.
    CashDrawerOpened => "cash.drawer.opened", version = 1 {
        /// Whether it opened outside a sale, which needs a permission and a reason.
        standalone: bool,
        /// Why, for a standalone opening.
        reason_code_id: Option<ReasonCodeId>,
        /// The till whose drawer this is, as [`CashShiftOpened::terminal_device_id`] says.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        terminal_device_id: Option<DeviceId>,
    },
    /// Cash was added to the drawer.
    CashDrawerPaidIn => "cash.drawer.paid_in", version = 1 {
        /// How much.
        amount: Money,
        /// Why.
        reason_code_id: ReasonCodeId,
        /// The till whose drawer this is, as [`CashShiftOpened::terminal_device_id`] says.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        terminal_device_id: Option<DeviceId>,
    },
    /// Cash was removed from the drawer.
    CashDrawerPaidOut => "cash.drawer.paid_out", version = 1 {
        /// How much.
        amount: Money,
        /// Why.
        reason_code_id: ReasonCodeId,
        /// The till whose drawer this is, as [`CashShiftOpened::terminal_device_id`] says.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        terminal_device_id: Option<DeviceId>,
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
        /// The supplier the goods came from, when recorded — a reference to a supplier authored in the
        /// `inventory` config node ([ADR-0079](../../../docs/adr/0079-inventory-and-suppliers.md), M6).
        /// Optional and additive: a receipt that names no supplier (or a producer that predates this
        /// field) decodes to `None`. It is a reference only — the full purchasing relationship stays in
        /// the ERP (§19). The goods-in flow that emits a receipt is the flagged edge follow-up.
        supplier_id: Option<SupplierId>,
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
    /// A store published the head of its hash chain
    /// ([ADR-0131](../../../docs/adr/0131-a-chained-event-log.md) decision 4).
    ///
    /// The anchor. A hash chain verifies happily after its tail has been cut off, and after an
    /// edit whose later links were all re-derived — the algorithm is in the source, so anyone
    /// holding the file can rebuild a chain that checks out. What a store **cannot** do is rewrite
    /// what the cloud has already seen, and this is that record.
    ///
    /// Published at shift close: a reconciliation point a human already attends to, and one that
    /// bounds how much history a store could rewrite unobserved to a single shift. It follows that
    /// the guarantee reaches back only to the **last anchor**, not to the last event.
    ///
    /// The head it carries is the chain as it stood **before this event was written**, because a
    /// record cannot contain its own hash. A sale landing between the read and the commit leaves
    /// the anchor a record or two behind, which is safe — it still extends the previous anchor, and
    /// the next one covers the gap.
    StoreChainAnchored => "store.chain.anchored", version = 1 {
        /// How many records the chain held when this was taken.
        chain_seq: u64,
        /// The hash of the last record.
        chain_head: ChainHash,
        /// How many records carry no chain because they predate it. Not a fault, and reported so
        /// the cloud can tell a store that upgraded mid-life from one that lost its history.
        unchained: u64,
    },
    /// A device finished activation and became able to trade.
    DeviceActivationCompleted => "device.activation.completed", version = 1 {
        /// The device that was activated, which may differ from the reporting device.
        activated_device_id: DeviceId,
    },
    /// A store admitted a device by pairing it
    /// ([ADR-0118](../../../docs/adr/0118-one-credential-per-box-and-the-cloud-learns.md) §4).
    ///
    /// Declared beyond the specification's list, and the reason is the one ADR-0041 §55-57 rejected
    /// and this record argued back: a tablet paired during a WAN outage was invisible to the cloud
    /// **forever**, so the fleet console's picture of a store's tills was literally zero rows. This
    /// rides the durable outbox, so an admission made with the cable unplugged reaches the cloud
    /// when the link returns.
    ///
    /// # It names devices and never an employee
    ///
    /// Deliberately, and it is decision 5 of that record rather than an omission. An employee
    /// identity here would create a durable, central, cross-border, attributable record of
    /// managerial activity — needing a lawful basis, a retention period, a DPIA and a transfer basis
    /// under Decree 13/2023, and failing GDPR's prior question, because the purpose (*the cloud
    /// knows which tills a store admitted, and can revoke one*) is served in full by device ids.
    /// Who admitted a device is recorded on the store's own disk. A v2 carrying the actor would be
    /// its own record, and this one does not pre-authorise it.
    DeviceAdmissionGranted => "device.admission.granted", version = 1 {
        /// The device that was admitted — the id the **edge** minted at redemption, which is a
        /// separate identity from the cloud's approved-device registry.
        admitted_device_id: DeviceId,
        /// The paired device whose signed-in manager minted the code, where one did. `None` for the
        /// code a boot announces, which no device authorised because none existed yet.
        admitted_by_device_id: Option<DeviceId>,
    },
    /// A store retired a device's admission
    /// ([ADR-0118](../../../docs/adr/0118-one-credential-per-box-and-the-cloud-learns.md) §4).
    ///
    /// Declared beyond the specification's list, and paired with [`DeviceAdmissionGranted`] because a
    /// roster that only ever grows is not a roster. One event per device: the break-glass that
    /// retires every device at once emits one of these each, so a reader has a single shape to
    /// handle and never has to interpret an absent field as *all of them*.
    DeviceAdmissionRevoked => "device.admission.revoked", version = 1 {
        /// The device whose token stops authenticating.
        revoked_device_id: DeviceId,
    },
    /// A release reached a store.
    FleetUpdateRolledOut => "fleet.update.rolled_out", version = 1 {
        /// The release.
        release_tag: ReleaseTag,
        /// Which ring the store is in.
        ring: u8,
    },
}

/// One fee as a settled bill records it
/// ([ADR-0159](../../../docs/adr/0159-a-fee-is-configuration.md) decision 4): which fee, under
/// which code and name, what it charged, and the tax on it.
///
/// The name is the rule's own as the bill froze it, so a copy of the receipt prints what the
/// original did after the fee is renamed, as a line's name does (ADR-0129, ADR-0144). No event
/// carries a translation.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct BillFeeLine {
    /// The fee rule that charged it.
    pub fee_id: FeeId,
    /// The rule's code, what a report groups fees by.
    pub code: FeeCode,
    /// The rule's own name, as the bill froze it.
    pub display_name: DisplayName,
    /// What it charged.
    pub amount: Money,
    /// The tax on it: its part of the tax of every class it is taxed at. Zero for a fee that is
    /// not taxed.
    pub tax: Money,
    /// That tax by component
    /// ([ADR-0168](../../../docs/adr/0168-a-settled-bill-records-its-tax-components.md) decision 3):
    /// the tax on each of its class shares split as that class's tax line is, then summed by name
    /// and rate, so they sum to `tax`.
    ///
    /// Left out when empty, and absent is not zero: it reads as *not split*, which is every fee
    /// charged before this field existed and every fee taxed at rates with no components. A
    /// reader that meets a name it cannot read takes the fee as not split too, and still reads the
    /// bill.
    #[serde(
        default,
        skip_serializing_if = "Vec::is_empty",
        deserialize_with = "read_components"
    )]
    pub tax_components: Vec<BillTaxComponent>,
}

// The personal-data barrier for a part of a payload, field by field, and its fields in the snapshot.
crate::envelope::payload_part!(BillFeeLine {
    fee_id,
    code,
    display_name,
    amount,
    tax,
    tax_components,
});

/// One class's tax as a settled bill records it (ADR-0159 decision 4, roadmap-v3 B4.1): the base,
/// the rate and the tax, which rounded once on that whole base (ADR-0028), and that tax by component
/// where the rate has components (ADR-0104, ADR-0168).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct BillTaxLine {
    /// The class.
    pub tax_class_id: TaxClassId,
    /// The base the rate was applied to: the lines after discounts and comps, plus every charge
    /// taxed at this class.
    pub taxable_base: Money,
    /// The rate in force, in basis points.
    pub rate_basis_points: u32,
    /// The class's tax.
    pub tax: Money,
    /// That tax by component, in the order the `tax` node lists them
    /// ([ADR-0168](../../../docs/adr/0168-a-settled-bill-records-its-tax-components.md) decision 3):
    /// allocated by their rates and summing exactly to `tax`, with the residual on the last, as
    /// ADR-0104 splits it.
    ///
    /// Left out when empty, and absent is not zero: it reads as *not split*, which is every bill
    /// settled before this field existed and every rate with no components, as in Vietnam and Japan.
    /// A reader that meets a name it cannot read takes the line as not split too, and still reads
    /// the bill.
    #[serde(
        default,
        skip_serializing_if = "Vec::is_empty",
        deserialize_with = "read_components"
    )]
    pub components: Vec<BillTaxComponent>,
}

// The personal-data barrier for a part of a payload, field by field, and its fields in the snapshot.
crate::envelope::payload_part!(BillTaxLine {
    tax_class_id,
    taxable_base,
    rate_basis_points,
    tax,
    components,
});

/// One named part of a settled tax line's tax, or of a fee's
/// ([ADR-0168](../../../docs/adr/0168-a-settled-bill-records-its-tax-components.md) decision 3):
/// `CGST` at 9 % and the tax it accounts for.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct BillTaxComponent {
    /// The component, as the `tax` node names it.
    pub name: TaxComponentName,
    /// Its part of the rate, in basis points.
    pub rate_basis_points: u32,
    /// Its part of the tax.
    pub tax: Money,
}

// The personal-data barrier for a part of a payload, field by field, and its fields in the snapshot.
crate::envelope::payload_part!(BillTaxComponent {
    name,
    rate_basis_points,
    tax,
});

/// Reads a line's components, all of them or none: a list with a name that is not a
/// [`TaxComponentName`] reads as empty, *not split*, and never as part of a split.
///
/// Lenient where [`TaxComponentName`] alone is strict, so a token a later release widens costs an
/// older reader the breakdown and not the bill: a settle it could not decode would leave the
/// cloud's rollup without the bill's revenue. Every other field reads as strictly as before.
fn read_components<'de, D>(deserializer: D) -> Result<Vec<BillTaxComponent>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    /// A component as the wire carries it. Its name is text only until it is checked, and a name
    /// that fails the check is dropped with the list.
    #[derive(Deserialize)]
    struct Unchecked {
        name: String,
        rate_basis_points: u32,
        tax: Money,
    }

    let unchecked = Vec::<Unchecked>::deserialize(deserializer)?;
    Ok(unchecked
        .into_iter()
        .map(|component| {
            TaxComponentName::new(&component.name).map(|name| BillTaxComponent {
                name,
                rate_basis_points: component.rate_basis_points,
                tax: component.tax,
            })
        })
        .collect::<Result<Vec<_>, _>>()
        .unwrap_or_default())
}

#[cfg(test)]
mod tests;
