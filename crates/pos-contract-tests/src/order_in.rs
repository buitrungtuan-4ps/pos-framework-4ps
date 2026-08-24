// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The `OrderIn` suite.
//!
//! The unusual one. Every other suite in this crate is aimed at an adapter somebody else may have
//! written; this port is implemented by `pos_edge` and `pos_cloud`, so these cases are the
//! *specification* of what a marketplace adapter, `POST /v1/orders` and the QR module may rely on.
//! Getting it wrong breaks three callers at once, which is the flip side of the reuse that makes QR
//! ordering nearly free ([ADR-0012](../../../docs/adr/0012-qr-ordering-via-cloud.md)).
//!
//! [`the_stores_price_wins`] is the case with margin attached. A marketplace's cached price can be
//! a day stale; honouring it silently loses money on every order until somebody notices, and
//! refusing the order loses a sale the store wanted. Reporting the difference is the only option
//! that does neither.

use pos_ports::PortName;
use pos_ports::order_in::{ExternalReference, InboundOrder, InboundOrderLine, OrderIn};
use pos_proto::money::Money;
use pos_proto::wire_enum::Open;
use pos_proto::{ErrorStatus, MenuItemId, Quantity, SalesChannel, StoreId};

use crate::harness::OrderInHarness;
use crate::{CaseFailure, Obligation, fixtures};

/// Emits every `OrderIn` case as a `#[test]`.
#[macro_export]
macro_rules! order_in_suite {
    ($harness:expr, $block_on:path) => {
        $crate::contract_cases! {
            harness = $harness,
            block_on = $block_on,
            port = $crate::__PORT_ORDER_IN,
            module = order_in,
            cases = [
                accepts_an_order,
                is_idempotent_by_channel_and_reference,
                treats_the_same_reference_on_two_channels_as_two_orders,
                the_stores_price_wins,
                refuses_an_unknown_menu_item,
                refuses_an_order_with_no_lines,
                looks_up_what_a_reference_produced,
            ]
        }
    };
}

fn idempotency() -> Obligation {
    Obligation::new(
        PortName::OrderIn,
        "idempotent by channel and external reference",
    )
}

fn pricing() -> Obligation {
    Obligation::new(PortName::OrderIn, "the store's price wins")
}

fn validation() -> Obligation {
    Obligation::new(
        PortName::OrderIn,
        "an unknown item is refused, not substituted",
    )
}

fn reference(text: &str) -> Result<ExternalReference, CaseFailure> {
    ExternalReference::parse(text)
        .map_err(|error| CaseFailure::new(format!("fixture reference `{text}`: {error}")))
}

fn order(
    store_id: StoreId,
    external_reference: ExternalReference,
    channel: SalesChannel,
    menu_item_id: MenuItemId,
    quoted: Option<Money>,
) -> InboundOrder {
    InboundOrder {
        external_reference,
        sales_channel: Open::from_known(channel),
        store_id,
        table_id: None,
        subject_id: None,
        lines: vec![InboundOrderLine {
            menu_item_id,
            quantity: Quantity::ONE,
            modifier_menu_item_ids: Vec::new(),
            quoted_unit_price: quoted,
            note: None,
        }],
        placed_at: fixtures::instant(),
    }
}

/// The happy path, with the store's own total.
///
/// # Errors
///
/// [`CaseFailure`] if the obligation does not hold.
pub async fn accepts_an_order<H: OrderInHarness>(harness: &H) -> Result<(), CaseFailure> {
    let intake = harness.fresh().await?;
    let (menu_item_id, price) = harness.known_menu_item();
    let accepted = intake
        .submit(&order(
            harness.store_id(),
            reference("GF-1")?,
            SalesChannel::Delivery,
            menu_item_id,
            None,
        ))
        .await?;
    let obligation = idempotency();
    obligation.require(accepted.created, "a first submission creates the order")?;
    obligation.require_eq(
        &accepted.total,
        &price,
        "and the total comes from the store's own menu",
    )
}

/// A retry returns the same order and says it did not create it.
///
/// # Errors
///
/// [`CaseFailure`] if the obligation does not hold.
pub async fn is_idempotent_by_channel_and_reference<H: OrderInHarness>(
    harness: &H,
) -> Result<(), CaseFailure> {
    let intake = harness.fresh().await?;
    let (menu_item_id, _) = harness.known_menu_item();
    let submission = order(
        harness.store_id(),
        reference("GF-1")?,
        SalesChannel::Delivery,
        menu_item_id,
        None,
    );

    let first = intake.submit(&submission).await?;
    let second = intake.submit(&submission).await?;
    let obligation = idempotency();
    obligation.require_eq(
        &second.order_id,
        &first.order_id,
        "a retried submission returns the original order, not a second one in the kitchen",
    )?;
    obligation.require(
        !second.created,
        "and reports that it did not create it, so a caller can tell a genuine duplicate from its \
         own retry",
    )
}

/// The channel scopes the key.
///
/// # Errors
///
/// [`CaseFailure`] if the obligation does not hold.
pub async fn treats_the_same_reference_on_two_channels_as_two_orders<H: OrderInHarness>(
    harness: &H,
) -> Result<(), CaseFailure> {
    let intake = harness.fresh().await?;
    let (menu_item_id, _) = harness.known_menu_item();

    let delivery = intake
        .submit(&order(
            harness.store_id(),
            reference("1001")?,
            SalesChannel::Delivery,
            menu_item_id,
            None,
        ))
        .await?;
    let takeaway = intake
        .submit(&order(
            harness.store_id(),
            reference("1001")?,
            SalesChannel::Takeaway,
            menu_item_id,
            None,
        ))
        .await?;

    let obligation = idempotency();
    obligation.require(
        delivery.order_id != takeaway.order_id,
        "two channels using the same reference are two orders. Nothing stops a marketplace and a \
         till from both numbering an order 1001, and collapsing them would silently drop a sale",
    )?;
    obligation.require(takeaway.created, "and the second one was created")
}

/// A stale quoted price is reported, not honoured or refused.
///
/// # Errors
///
/// [`CaseFailure`] if the obligation does not hold.
pub async fn the_stores_price_wins<H: OrderInHarness>(harness: &H) -> Result<(), CaseFailure> {
    let intake = harness.fresh().await?;
    let (menu_item_id, price) = harness.known_menu_item();
    let obligation = pricing();

    // A quote well below the menu price, which is what a day-old cached menu looks like.
    let stale = price
        .checked_sub(Money::new(price.currency_code, 1))
        .map_err(|error| CaseFailure::new(format!("fixture price: {error}")))?;
    let accepted = intake
        .submit(&order(
            harness.store_id(),
            reference("GF-1")?,
            SalesChannel::Delivery,
            menu_item_id,
            Some(stale),
        ))
        .await?;

    obligation.require_eq(
        &accepted.total,
        &price,
        "the store charges its own price, not the caller's quote",
    )?;
    obligation.require(
        accepted.repriced,
        "and says the quote differed. Silently honouring a stale price loses margin on every \
         order until somebody notices; refusing loses a sale the store wanted",
    )?;

    let matching = intake
        .submit(&order(
            harness.store_id(),
            reference("GF-2")?,
            SalesChannel::Delivery,
            menu_item_id,
            Some(price),
        ))
        .await?;
    obligation.require(
        !matching.repriced,
        "and a quote that agrees is not reported as repriced, or the flag means nothing",
    )
}

/// Nothing here guesses what a caller meant.
///
/// # Errors
///
/// [`CaseFailure`] if the obligation does not hold.
pub async fn refuses_an_unknown_menu_item<H: OrderInHarness>(
    harness: &H,
) -> Result<(), CaseFailure> {
    let intake = harness.fresh().await?;
    validation().require_error(
        intake
            .submit(&order(
                harness.store_id(),
                reference("GF-1")?,
                SalesChannel::Delivery,
                harness.unknown_menu_item(),
                None,
            ))
            .await,
        ErrorStatus::InvalidArgument,
        "an unknown menu item is refused. Substituting the closest match is how a kitchen makes \
         the wrong thing and a customer is charged for it",
    )
}

/// An empty order is not an order.
///
/// # Errors
///
/// [`CaseFailure`] if the obligation does not hold.
pub async fn refuses_an_order_with_no_lines<H: OrderInHarness>(
    harness: &H,
) -> Result<(), CaseFailure> {
    let intake = harness.fresh().await?;
    let (menu_item_id, _) = harness.known_menu_item();
    let empty = InboundOrder {
        lines: Vec::new(),
        ..order(
            harness.store_id(),
            reference("GF-1")?,
            SalesChannel::Delivery,
            menu_item_id,
            None,
        )
    };
    validation().require_error(
        intake.submit(&empty).await,
        ErrorStatus::InvalidArgument,
        "an order with no lines is refused rather than opened empty — a public endpoint receives \
         these, and an empty order in the kitchen queue is a support call",
    )
}

/// A caller whose submit timed out can ask instead of retrying.
///
/// # Errors
///
/// [`CaseFailure`] if the obligation does not hold.
pub async fn looks_up_what_a_reference_produced<H: OrderInHarness>(
    harness: &H,
) -> Result<(), CaseFailure> {
    let intake = harness.fresh().await?;
    let (menu_item_id, _) = harness.known_menu_item();
    let channel = Open::from_known(SalesChannel::Delivery);
    let obligation = idempotency();

    let absent = intake
        .look_up(harness.store_id(), channel.clone(), &reference("GF-1")?)
        .await?;
    obligation.require(
        absent.is_none(),
        "a reference nothing was submitted under reports None rather than erroring",
    )?;

    let submitted = intake
        .submit(&order(
            harness.store_id(),
            reference("GF-1")?,
            SalesChannel::Delivery,
            menu_item_id,
            None,
        ))
        .await?;
    let found = intake
        .look_up(harness.store_id(), channel, &reference("GF-1")?)
        .await?;
    let found = obligation.require_nth(found.as_slice(), 0, "the looked-up order")?;
    obligation.require_eq(
        &found.order_id,
        &submitted.order_id,
        "and after submitting, the reference resolves to the order. This is the path for a caller \
         whose submit timed out, where a retry would otherwise be indistinguishable from a second \
         order",
    )
}
