// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The `DeliveryVendor` suite.
//!
//! [`closes_the_store_when_it_goes_offline`] is the commercially important case. A marketplace that
//! keeps taking orders nobody can see produces cancellations, and cancellations cost the store its
//! ranking — so `docs/architecture.md` §6.1's "store offline ⇒ vendor sees busy" is not a courtesy,
//! it is what stops an outage becoming a lasting commercial problem.

use pos_ports::PortName;
use pos_ports::delivery::{BusyMode, DeliveryVendor, PrepTime};
use pos_proto::{ErrorStatus, ReasonCodeId, Ulid};

use crate::harness::DeliveryVendorHarness;
use crate::{CaseFailure, Obligation};

/// Emits every `DeliveryVendor` case as a `#[test]`.
#[macro_export]
macro_rules! delivery_vendor_suite {
    ($harness:expr, $block_on:path) => {
        $crate::contract_cases! {
            harness = $harness,
            block_on = $block_on,
            port = $crate::__PORT_DELIVERY_VENDOR,
            module = delivery,
            cases = [
                accepts_an_order,
                rejects_an_order_with_a_managed_reason,
                refuses_to_contradict_an_earlier_decision,
                refuses_a_decision_after_the_window_closes,
                is_idempotent_on_a_repeated_decision,
                closes_the_store_when_it_goes_offline,
                lists_orders_still_awaiting_a_decision,
            ]
        }
    };
}

fn decisions() -> Obligation {
    Obligation::new(PortName::DeliveryVendor, "one decision per order")
}

fn idempotency() -> Obligation {
    Obligation::new(PortName::DeliveryVendor, "idempotency by vendor reference")
}

fn busy_signal() -> Obligation {
    Obligation::new(PortName::DeliveryVendor, "busy mode is the offline signal")
}

fn reason() -> ReasonCodeId {
    ReasonCodeId::new(Ulid::from_u128(1))
}

/// The happy path, and the promise that goes with it.
///
/// # Errors
///
/// [`CaseFailure`] if the obligation does not hold.
pub async fn accepts_an_order<H: DeliveryVendorHarness>(harness: &H) -> Result<(), CaseFailure> {
    let vendor = harness.fresh().await?;
    let order = harness.stage_order(&vendor).await?;
    vendor.accept(&order, PrepTime::minutes(20)).await?;
    vendor.mark_ready(&order).await?;
    Ok(())
}

/// Rejection needs a reason from the managed list.
///
/// # Errors
///
/// [`CaseFailure`] if the obligation does not hold.
pub async fn rejects_an_order_with_a_managed_reason<H: DeliveryVendorHarness>(
    harness: &H,
) -> Result<(), CaseFailure> {
    let vendor = harness.fresh().await?;
    let order = harness.stage_order(&vendor).await?;
    vendor.reject(&order, reason()).await?;
    Ok(())
}

/// Accept-then-reject, and reject-then-accept, both fail.
///
/// # Errors
///
/// [`CaseFailure`] if the obligation does not hold.
pub async fn refuses_to_contradict_an_earlier_decision<H: DeliveryVendorHarness>(
    harness: &H,
) -> Result<(), CaseFailure> {
    let vendor = harness.fresh().await?;
    let obligation = decisions();

    let accepted = harness.stage_order(&vendor).await?;
    vendor.accept(&accepted, PrepTime::minutes(20)).await?;
    obligation.require_error(
        vendor.reject(&accepted, reason()).await,
        ErrorStatus::FailedPrecondition,
        "rejecting an accepted order must fail. A vendor that quietly allows it leaves the store \
         and the marketplace disagreeing about whether food is being cooked",
    )?;

    let rejected = harness.stage_order(&vendor).await?;
    vendor.reject(&rejected, reason()).await?;
    obligation.require_error(
        vendor.accept(&rejected, PrepTime::minutes(20)).await,
        ErrorStatus::FailedPrecondition,
        "and accepting a rejected order must fail too",
    )?;

    // Ready before accepted is the third contradiction, and the one most likely to be reached by a
    // race between a kitchen display and an acceptance that has not landed yet.
    let untouched = harness.stage_order(&vendor).await?;
    obligation.require_error(
        vendor.mark_ready(&untouched).await,
        ErrorStatus::FailedPrecondition,
        "and an order that was never accepted cannot be ready",
    )
}

/// A closed window is the vendor's decision.
///
/// # Errors
///
/// [`CaseFailure`] if the obligation does not hold.
pub async fn refuses_a_decision_after_the_window_closes<H: DeliveryVendorHarness>(
    harness: &H,
) -> Result<(), CaseFailure> {
    let vendor = harness.fresh().await?;
    let expired = harness.stage_expired_order(&vendor).await?;
    decisions().require_error(
        vendor.accept(&expired, PrepTime::minutes(20)).await,
        ErrorStatus::FailedPrecondition,
        "a missed window is a precondition failure, not unavailability. Retrying into a closed \
         window is how an adapter spends its rate limit arguing with a decision already made",
    )
}

/// Marketplace APIs retry, so repeats must be harmless.
///
/// # Errors
///
/// [`CaseFailure`] if the obligation does not hold.
pub async fn is_idempotent_on_a_repeated_decision<H: DeliveryVendorHarness>(
    harness: &H,
) -> Result<(), CaseFailure> {
    let vendor = harness.fresh().await?;
    let order = harness.stage_order(&vendor).await?;
    vendor.accept(&order, PrepTime::minutes(20)).await?;
    idempotency().require(
        vendor.accept(&order, PrepTime::minutes(20)).await.is_ok(),
        "accepting twice is one acceptance. None of these APIs promise exactly-once, so a repeat \
         is ordinary traffic rather than a contradiction",
    )
}

/// Going offline closes the store on the marketplace.
///
/// # Errors
///
/// [`CaseFailure`] if the obligation does not hold.
pub async fn closes_the_store_when_it_goes_offline<H: DeliveryVendorHarness>(
    harness: &H,
) -> Result<(), CaseFailure> {
    let vendor = harness.fresh().await?;
    let obligation = busy_signal();

    vendor
        .set_busy(harness.store_id(), BusyMode::Closed)
        .await?;
    let observed = harness.busy_mode(&vendor).await?;
    obligation.require(
        !observed.accepts_orders(),
        format!(
            "after setting Closed the vendor must stop sending orders; it reports {observed:?}. A \
             marketplace still taking orders nobody can see produces cancellations, and \
             cancellations cost the store its ranking"
        ),
    )?;

    // Repeated, because the store re-asserts on reconnect rather than tracking whether it already
    // did — and a second Closed must not error.
    vendor
        .set_busy(harness.store_id(), BusyMode::Closed)
        .await?;

    vendor
        .set_busy(
            harness.store_id(),
            BusyMode::Busy {
                prep_time: PrepTime::minutes(45),
            },
        )
        .await?;
    let busy = harness.busy_mode(&vendor).await?;
    obligation.require(
        busy.accepts_orders(),
        "busy still takes orders, just slower — only Closed stops them",
    )
}

/// Reconnecting has to find the windows that were running down.
///
/// # Errors
///
/// [`CaseFailure`] if the obligation does not hold.
pub async fn lists_orders_still_awaiting_a_decision<H: DeliveryVendorHarness>(
    harness: &H,
) -> Result<(), CaseFailure> {
    let vendor = harness.fresh().await?;
    let obligation = decisions();
    obligation.require_len(
        &vendor.pending_decisions(harness.store_id()).await?,
        0,
        "a fresh vendor has nothing pending",
    )?;

    let staged = harness.stage_order(&vendor).await?;
    let pending = vendor.pending_decisions(harness.store_id()).await?;
    obligation.require(
        pending
            .iter()
            .any(|decision| decision.vendor_order_ref == staged),
        "a staged order awaiting a decision is listed. This is what a store polls on reconnect: a \
         push that arrived while it was offline is a window running down with nobody watching",
    )?;

    vendor.accept(&staged, PrepTime::minutes(20)).await?;
    let after = vendor.pending_decisions(harness.store_id()).await?;
    obligation.require(
        !after
            .iter()
            .any(|decision| decision.vendor_order_ref == staged),
        "and a decided order drops off the list",
    )
}
