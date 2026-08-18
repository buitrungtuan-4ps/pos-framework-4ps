// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! Orders originating outside the store.
//!
//! # This port points the other way
//!
//! The other fifteen are *driven* ports: the framework calls out to a database, a broker, a
//! printer. This one is a *driving* port — the application implements it, and `vendor-grab`,
//! `POST /v1/orders` and the QR ordering module call in.
//!
//! That inversion is the whole reason [ADR-0012](../../../docs/adr/0012-qr-ordering-via-cloud.md)
//! can say QR ordering is architecturally almost free. A guest scanning a table code is not a
//! new pipeline; it is a third caller of a path that already exists, and it inherits the
//! marketplace path's validation, idempotency, rate limiting and kitchen routing for nothing.
//! `docs/roadmap.md` P11 therefore builds this port **first** among the integrations.
//!
//! # Its contract suite tests our code, not a vendor's
//!
//! Which is unusual and worth stating: for every other port the suite is aimed at an adapter we
//! may not have written. Here the implementation is `pos_edge` or `pos_cloud`, so the suite is
//! the specification of what a caller may rely on. See
//! [ADR-0026](../../../docs/adr/0026-port-shapes.md) §5.
//!
//! # Idempotency is the caller's key, not ours
//!
//! Every other port deduplicates on a ULID the framework minted. A marketplace has not got one
//! — it has its own order reference, it retries on its own schedule, and its retry must not
//! produce a second order in the kitchen. So the key is
//! [`InboundOrder::external_reference`], scoped to the channel that supplied it, and two
//! channels using the same string are two different orders.

use core::fmt;
use core::future::Future;

use pos_proto::money::Money;
use pos_proto::wire_enum::Open;
use pos_proto::{
    GuestNote, MenuItemId, OrderId, Quantity, SalesChannel, StoreId, SubjectId, TableId, Timestamp,
};

use crate::error::PortError;

/// The caller's own reference for an order.
///
/// The idempotency key, scoped by channel. Not a ULID, because the caller did not mint one — see
/// this module's documentation.
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ExternalReference(Box<str>);

impl ExternalReference {
    /// The longest reference accepted.
    ///
    /// Bounded because this string is a database key and an unbounded one is a denial-of-service
    /// vector on a public endpoint.
    pub const MAX_LEN: usize = 128;

    /// Wraps a caller's reference.
    ///
    /// # Errors
    ///
    /// [`PortError::invalid_argument`] if empty or longer than [`Self::MAX_LEN`].
    pub fn parse(reference: &str) -> Result<Self, PortError> {
        if reference.is_empty() {
            return Err(PortError::invalid_argument(
                crate::PortName::OrderIn,
                "external_reference must not be empty",
            ));
        }
        if reference.len() > Self::MAX_LEN {
            return Err(PortError::invalid_argument(
                crate::PortName::OrderIn,
                "external_reference is too long",
            ));
        }
        Ok(Self(reference.into()))
    }

    /// The reference as text.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for ExternalReference {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl fmt::Debug for ExternalReference {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "ExternalReference({})", self.0)
    }
}

/// One requested item.
#[derive(Clone, Debug, PartialEq)]
pub struct InboundOrderLine {
    /// What was ordered. A menu item the store knows, not free text — an inbound channel that
    /// cannot name a menu item cannot place an order, and guessing from a description is how a
    /// kitchen makes the wrong thing.
    pub menu_item_id: MenuItemId,
    /// How many, in thousandths, so a half portion is expressible.
    pub quantity: Quantity,
    /// Chosen modifiers, as menu item identifiers.
    pub modifier_menu_item_ids: Vec<MenuItemId>,
    /// The price the caller believes applies.
    ///
    /// Advisory. The store re-prices from its own menu, and a mismatch is reported in
    /// [`OrderAcceptance::repriced`] rather than accepted — a marketplace's cached price is not
    /// authority over what a store charges, and silently honouring it is how a menu update loses
    /// money for a day.
    pub quoted_unit_price: Option<Money>,
    /// What the guest asked for in words. Free text, so it stays at the store and never enters
    /// the event log — see [`GuestNote`].
    pub note: Option<GuestNote>,
}

/// An order arriving from outside.
#[derive(Clone, Debug, PartialEq)]
pub struct InboundOrder {
    /// The caller's reference, and the idempotency key.
    pub external_reference: ExternalReference,
    /// Which channel it came from. Scopes the idempotency key, and decides the tax rate, since
    /// `docs/pos-spec.md` §5 keys tax on the sales channel.
    pub sales_channel: Open<SalesChannel>,
    /// Which store is to make it.
    pub store_id: StoreId,
    /// Which table, for a QR order. `None` for delivery and for the public API.
    pub table_id: Option<TableId>,
    /// The recipient, when there is one and their details are held in the side table.
    pub subject_id: Option<SubjectId>,
    /// What was ordered.
    pub lines: Vec<InboundOrderLine>,
    /// When the caller says it was placed.
    pub placed_at: Timestamp,
}

/// What became of an inbound order.
#[derive(Clone, Debug, PartialEq)]
pub struct OrderAcceptance {
    /// The framework's identifier for it.
    pub order_id: OrderId,
    /// Whether this call created it, or found one already there.
    ///
    /// A retry reports `false`, and a caller can use that to tell a genuine duplicate from its
    /// own retry — which is the difference between an operational curiosity and a bug worth
    /// chasing.
    pub created: bool,
    /// The queue number, for a channel that issues one.
    ///
    /// `docs/pos-spec.md` §10: a counter cafe issues a queue number that **resets daily**, and
    /// it is not the gapless store-lifetime receipt counter. Two different numbers, and
    /// conflating them is a mistake that surfaces at midnight.
    pub queue_number: Option<u32>,
    /// The store's own total, which is authoritative.
    pub total: Money,
    /// Whether the store's price differed from the caller's quote.
    ///
    /// Reported rather than refused: rejecting a marketplace order over a stale cached price
    /// loses a sale the store wanted, and accepting the stale price loses margin. Telling the
    /// caller is the only option that does neither.
    pub repriced: bool,
    /// Whether a member of staff must confirm before the kitchen sees it.
    ///
    /// True by default for QR ordering (ADR-0012), which is what stops a passer-by ordering
    /// forty pizzas to a table they are not sitting at.
    pub awaiting_staff_confirmation: bool,
}

/// Accepts orders from outside the store.
///
/// # Contract
///
/// 1. **Idempotent by `(sales_channel, external_reference)`.** A repeat returns the same
///    [`OrderAcceptance::order_id`] with `created: false`. The same reference on two channels is
///    two orders.
/// 2. **The store's price wins.** An implementation re-prices from its own menu and sets
///    [`OrderAcceptance::repriced`]; it never charges the caller's quoted price.
/// 3. **An unknown menu item is [`PortError::invalid_argument`], not a substitution.** Nothing
///    here may guess what a caller meant.
/// 4. **A guest note never reaches the event log.** [`GuestNote`] exists to make that
///    structural: the note stays on the local order record and the log carries only that one was
///    present.
/// 5. **Accepting must not require the cloud.** A QR order is cloud-mediated by design and a
///    marketplace order arrives over the internet, but an implementation at the edge must accept
///    an order with no cloud reachable — otherwise ADR-0001 is broken by a channel rather than
///    by a component.
pub trait OrderIn: Send + Sync {
    /// Submits an order.
    ///
    /// # Errors
    ///
    /// [`PortError::invalid_argument`] for an unknown menu item, an empty line list, or a
    /// malformed reference; [`PortError::failed_precondition`] if the store is closed, the table
    /// is unknown, or a required capability is disabled;
    /// [`PortError::resource_exhausted`] if a per-table or per-channel rate limit is hit — which
    /// ADR-0012 requires for QR ordering; [`PortError::already_exists`] only when the same
    /// reference arrives with **different** contents, since an identical repeat succeeds.
    fn submit(
        &self,
        order: &InboundOrder,
    ) -> impl Future<Output = Result<OrderAcceptance, PortError>> + Send;

    /// Looks up what a reference produced, without submitting anything.
    ///
    /// The resolution path for a caller whose submit timed out: it can ask rather than retry,
    /// which matters when the retry would otherwise be indistinguishable from a second order.
    ///
    /// # Errors
    ///
    /// [`PortError::unavailable`] if the store cannot be reached.
    fn look_up(
        &self,
        store_id: StoreId,
        sales_channel: Open<SalesChannel>,
        external_reference: &ExternalReference,
    ) -> impl Future<Output = Result<Option<OrderAcceptance>, PortError>> + Send;
}

#[cfg(test)]
mod tests {
    use super::ExternalReference;
    use pos_proto::error::ErrorStatus;

    #[test]
    fn a_reference_is_bounded_because_it_is_a_public_key() {
        assert!(ExternalReference::parse("GF-12345").is_ok());

        let empty = ExternalReference::parse("").expect_err("must be refused");
        assert_eq!(empty.status(), ErrorStatus::InvalidArgument);

        let long = "x".repeat(ExternalReference::MAX_LEN + 1);
        let too_long = ExternalReference::parse(&long).expect_err("must be refused");
        assert_eq!(too_long.status(), ErrorStatus::InvalidArgument);

        let at_limit = "x".repeat(ExternalReference::MAX_LEN);
        assert!(
            ExternalReference::parse(&at_limit).is_ok(),
            "the limit itself is allowed, so the boundary is not off by one"
        );
    }

    #[test]
    fn an_error_from_this_port_names_this_port() {
        // So the error mailbox and the per-adapter latency chart attribute it correctly rather
        // than to whichever caller happened to be holding it.
        let error = ExternalReference::parse("").expect_err("must be refused");
        assert_eq!(error.port(), crate::PortName::OrderIn);
    }

    #[test]
    fn a_reference_prints_for_a_support_conversation() {
        let reference = ExternalReference::parse("SPF-777").expect("valid");
        assert_eq!(reference.to_string(), "SPF-777");
        assert_eq!(format!("{reference:?}"), "ExternalReference(SPF-777)");
    }
}
