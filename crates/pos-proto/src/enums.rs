// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The enumerations that cross a boundary.
//!
//! Each is declared with [`wire_enum!`](crate::wire_enum), which supplies the
//! mandatory `*_UNSPECIFIED` zero value and derives every `UPPER_SNAKE_CASE` token
//! from one prefix, so a variant cannot be spelled one way in Rust and another on the
//! wire.
//!
//! These are the *closed* vocabularies — sets the framework owns and a tenant cannot
//! extend. Anything a tenant configures is data, not an enum: courses, tax classes,
//! void reasons, payment-method labels beyond these categories, and station names all
//! live in the configuration tree and are referenced by id.

use crate::wire_enum;

wire_enum! {
    /// Where an order came from.
    ///
    /// Prices may differ per channel — a marketplace price usually absorbs the
    /// vendor's commission — and tax may differ per channel too, which is why
    /// `store.tax.tax_class_rates` is keyed by it (`docs/pos-spec.md` §5).
    SalesChannel, prefix = "SALES_CHANNEL";
    /// Eaten in, against a table.
    DineIn = "DINE_IN",
    /// Collected at the counter.
    Takeaway = "TAKEAWAY",
    /// Arrived from a delivery marketplace.
    Delivery = "DELIVERY",
    /// Submitted by a guest scanning a table code.
    Qr = "QR",
    /// Created through the public API by an external channel.
    Api = "API",
}

wire_enum! {
    /// The lifecycle of an order.
    OrderState, prefix = "ORDER_STATE";
    /// Accepting lines.
    Open = "OPEN",
    /// Paid in full. Accepts no further lines.
    Settled = "SETTLED",
    /// Cancelled in its entirety.
    Voided = "VOIDED",
}

wire_enum! {
    /// The lifecycle of one line on an order.
    ///
    /// The transition that matters operationally is `Added → Fired`: stock is
    /// consumed at fire time, and after firing a line can only be voided with a
    /// reason and a permission, which prints a void ticket at that item's own
    /// station (`docs/pos-spec.md` §3).
    OrderLineState, prefix = "ORDER_LINE_STATE";
    /// On the order and freely editable.
    Added = "ADDED",
    /// Deliberately withheld from the kitchen until fired by hand.
    Held = "HELD",
    /// Sent to the kitchen. Stock has been consumed.
    Fired = "FIRED",
    /// Cancelled. If it had been fired, this recorded waste rather than returning
    /// stock.
    Voided = "VOIDED",
}

wire_enum! {
    /// The lifecycle of a bill.
    ///
    /// Distinct from [`OrderState`]: one order may be split across several bills, and
    /// several orders may be merged into one.
    BillState, prefix = "BILL_STATE";
    /// Open for payment.
    Open = "OPEN",
    /// Settled. This is a one-time transition — a second attempt returns
    /// `FAILED_PRECONDITION` (`docs/pos-spec.md` §14.4).
    Settled = "SETTLED",
    /// Voided after settlement, which requires a manager and a reason.
    Voided = "VOIDED",
}

wire_enum! {
    /// The state of a table on the floor plan.
    ///
    /// A table holds exactly one open order at a time.
    TableState, prefix = "TABLE_STATE";
    /// Available to seat.
    Free = "FREE",
    /// Serving guests.
    Occupied = "OCCUPIED",
    /// Guests have asked for the bill.
    AwaitingPayment = "AWAITING_PAYMENT",
    /// Paid and vacated, not yet cleaned.
    NeedsCleaning = "NEEDS_CLEANING",
}

wire_enum! {
    /// The lifecycle of a cash shift.
    ///
    /// `Counted` exists as its own state because the close is **blind**: the cashier
    /// enters the counted amount before the system reveals what it expected
    /// (`docs/pos-spec.md` §6). Folding the count into the close would make the
    /// blindness unverifiable afterwards, which defeats the control.
    ShiftState, prefix = "SHIFT_STATE";
    /// Trading, with a starting float recorded.
    Open = "OPEN",
    /// The counted amount is recorded; the variance has not been revealed.
    Counted = "COUNTED",
    /// Closed and locked. Accepts no further transactions.
    Closed = "CLOSED",
}

wire_enum! {
    /// How a bill was paid.
    ///
    /// Several may combine on one bill. `GiftCard` is a reserved slot: gift cards are
    /// out of scope for version one because they need an online balance ledger, and
    /// the enum value exists so adding them later does not renumber anything
    /// (`docs/pos-spec.md` §19).
    PaymentMethod, prefix = "PAYMENT_METHOD";
    /// Notes and coins.
    Cash = "CASH",
    /// A card terminal.
    Card = "CARD",
    /// A scan-to-pay wallet.
    Qr = "QR",
    /// A voucher, whose redemption is an atomic check-and-mark against the cloud.
    Voucher = "VOUCHER",
    /// Reserved. Not implemented in version one.
    GiftCard = "GIFT_CARD",
    /// Anything else, with a note.
    Other = "OTHER",
}

wire_enum! {
    /// What a payment attempt concluded.
    ///
    /// `Unknown` is **not** the same as `Unspecified`, and conflating them would be a
    /// costly mistake. `Unspecified` means this build did not understand the sender.
    /// `Unknown` is a real, expected outcome: the terminal was asked and could not
    /// say. A card may or may not have been charged.
    ///
    /// The specification requires that this branch always exist and that it park the
    /// bill for reconciliation rather than guess (`docs/pos-spec.md` §5). The user
    /// interface shows it amber with two guided exits, and it appears in the
    /// reconciliation list.
    PaymentOutcome, prefix = "PAYMENT_OUTCOME";
    /// The money moved.
    Captured = "CAPTURED",
    /// The money did not move, and the terminal said so.
    Declined = "DECLINED",
    /// Indeterminate. Resolve by reconciliation, never by assumption.
    Unknown = "UNKNOWN",
}

wire_enum! {
    /// Why a stock ledger entry exists.
    ///
    /// Five kinds, and the distinctions carry accounting weight. Consumption is
    /// automatic at fire time. `Waste` is what a void-after-fire records — it does
    /// **not** return stock, because the kitchen already used the ingredients.
    /// `Stocktake` records a counted quantity and a count time, and its delta is
    /// computed against the projection *at the moment of counting*, so sales during
    /// the count do not corrupt it (`docs/pos-spec.md` §8).
    StockLedgerEntryKind, prefix = "STOCK_LEDGER_ENTRY_KIND";
    /// Consumed by firing a line.
    Consumption = "CONSUMPTION",
    /// Goods received.
    Receipt = "RECEIPT",
    /// A manual correction.
    Adjustment = "ADJUSTMENT",
    /// Spoiled, dropped, or cancelled after the kitchen had started.
    Waste = "WASTE",
    /// A physical count.
    Stocktake = "STOCKTAKE",
}

wire_enum! {
    /// How a price reduction was granted.
    ///
    /// These three are deliberately separate, and `docs/pos-spec.md` §5 is explicit
    /// that accounting and fraud analysis treat them differently. A **discount**
    /// reduces the price. A **comp** gives the item away — it still consumes
    /// inventory and is recorded as cost. A **void** says the item never happened.
    /// Collapsing them into one "adjustment" concept is a data-model mistake that is
    /// expensive to undo.
    ReductionKind, prefix = "REDUCTION_KIND";
    /// The price came down.
    Discount = "DISCOUNT",
    /// Given away. Inventory is still consumed and cost is still recorded.
    Comp = "COMP",
    /// It never happened.
    Void = "VOID",
}

#[cfg(test)]
mod tests {
    use super::{
        BillState, OrderLineState, OrderState, PaymentMethod, PaymentOutcome, ReductionKind,
        SalesChannel, ShiftState, StockLedgerEntryKind, TableState,
    };
    use crate::wire_enum::{Open, WireEnum};

    /// Asserts the invariants every wire enum must satisfy.
    fn check<E: WireEnum + core::fmt::Debug>(expected_prefix: &str) {
        let unspecified = E::UNSPECIFIED.as_wire();
        assert_eq!(
            unspecified,
            format!("{expected_prefix}_UNSPECIFIED"),
            "the zero value must be {expected_prefix}_UNSPECIFIED"
        );
        assert_eq!(
            E::ALL.first().map(|first| first.as_wire()),
            Some(unspecified),
            "UNSPECIFIED must be the first variant"
        );
        for variant in E::ALL {
            let token = variant.as_wire();
            assert!(
                token.starts_with(expected_prefix),
                "{token} does not carry the {expected_prefix} prefix"
            );
            assert!(
                token
                    .bytes()
                    .all(|b| b.is_ascii_uppercase() || b == b'_' || b.is_ascii_digit()),
                "{token} is not UPPER_SNAKE_CASE"
            );
            assert_eq!(
                E::from_wire(token).map(E::as_wire),
                Some(token),
                "{token} does not round trip"
            );
        }
        // Unknown tokens degrade rather than fail, which is what makes adding a
        // variant a non-breaking change.
        let unknown = Open::<E>::parse(&format!("{expected_prefix}_FROM_THE_FUTURE"));
        assert!(unknown.is_unspecified() && unknown.is_unrecognised());
        assert!(unknown.require().is_err());
    }

    #[test]
    fn every_enum_satisfies_the_naming_standard() {
        check::<SalesChannel>("SALES_CHANNEL");
        check::<OrderState>("ORDER_STATE");
        check::<OrderLineState>("ORDER_LINE_STATE");
        check::<BillState>("BILL_STATE");
        check::<TableState>("TABLE_STATE");
        check::<ShiftState>("SHIFT_STATE");
        check::<PaymentMethod>("PAYMENT_METHOD");
        check::<PaymentOutcome>("PAYMENT_OUTCOME");
        check::<StockLedgerEntryKind>("STOCK_LEDGER_ENTRY_KIND");
        check::<ReductionKind>("REDUCTION_KIND");
    }

    #[test]
    fn the_tokens_documented_in_the_naming_standard_are_exact() {
        // `docs/naming-and-api.md` §3.3 lists these verbatim.
        assert_eq!(OrderState::Unspecified.as_wire(), "ORDER_STATE_UNSPECIFIED");
        assert_eq!(OrderState::Open.as_wire(), "ORDER_STATE_OPEN");
        assert_eq!(OrderState::Settled.as_wire(), "ORDER_STATE_SETTLED");
        assert_eq!(OrderState::Voided.as_wire(), "ORDER_STATE_VOIDED");
        assert_eq!(PaymentMethod::Cash.as_wire(), "PAYMENT_METHOD_CASH");
        assert_eq!(
            PaymentMethod::GiftCard.as_wire(),
            "PAYMENT_METHOD_GIFT_CARD"
        );
        assert_eq!(PaymentMethod::Other.as_wire(), "PAYMENT_METHOD_OTHER");
    }

    #[test]
    fn an_unknown_card_result_is_not_an_unspecified_one() {
        // A terminal that cannot say whether it charged the card is reporting a real
        // outcome, and the bill must park for reconciliation. Treating it as "absent"
        // would silently drop a payment that may have succeeded.
        let unknown = Open::from_known(PaymentOutcome::Unknown);
        assert!(!unknown.is_unspecified());
        assert_eq!(
            unknown.require().expect("a real outcome"),
            PaymentOutcome::Unknown
        );

        let absent = Open::<PaymentOutcome>::default();
        assert!(absent.is_unspecified());
        assert!(absent.require().is_err());
    }

    #[test]
    fn payment_methods_cover_the_documented_set() {
        // Six values including the reserved gift-card slot, even though version one
        // shows four buttons.
        assert_eq!(PaymentMethod::ALL.len(), 7, "six methods plus UNSPECIFIED");
    }
}
