// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The `ShippingDispatch` suite.
//!
//! [`refuses_to_cancel_a_completed_job`] is the case with money attached: a courier who has already
//! delivered cannot un-deliver, and a cancel that looks successful leaves the store expecting a
//! refund it will not get.

use pos_ports::PortName;
use pos_ports::shipping::{
    CourierJobRef, DeliveryContact, DeliveryRequest, ShipmentUpdate, ShippingDispatch,
};
use pos_proto::{ErrorStatus, OrderId, ShipmentId, ShipmentStatus, StoreId, SubjectId, Ulid};

use crate::harness::ShippingDispatchHarness;
use crate::{CaseFailure, Obligation, fixtures};

/// Emits every `ShippingDispatch` case as a `#[test]`.
#[macro_export]
macro_rules! shipping_dispatch_suite {
    ($harness:expr, $block_on:path) => {
        $crate::contract_cases! {
            harness = $harness,
            block_on = $block_on,
            port = $crate::__PORT_SHIPPING_DISPATCH,
            module = shipping,
            cases = [
                books_a_courier,
                is_idempotent_by_shipment_id,
                tracks_a_booked_job,
                cancels_a_booked_job,
                cancels_idempotently,
                refuses_to_cancel_a_completed_job,
                tracks_a_completed_job_for_reconciliation,
                reports_an_unknown_job_as_not_found,
            ]
        }
    };
}

fn booking() -> Obligation {
    Obligation::new(
        PortName::ShippingDispatch,
        "idempotency by shipment identifier",
    )
}

fn cancellation() -> Obligation {
    Obligation::new(PortName::ShippingDispatch, "cancel after completion fails")
}

fn tracking() -> Obligation {
    Obligation::new(PortName::ShippingDispatch, "track is safe to poll")
}

fn request(store_id: StoreId, seed: u32) -> DeliveryRequest {
    DeliveryRequest {
        shipment_id: ShipmentId::new(Ulid::from_u128(u128::from(seed))),
        store_id,
        order_id: OrderId::new(Ulid::from_u128(u128::from(seed))),
        contact: DeliveryContact {
            subject_id: SubjectId::new(Ulid::from_u128(u128::from(seed))),
            recipient_name: "Recipient".to_owned(),
            recipient_phone: "0900000000".to_owned(),
            delivery_address: "1 Test Street".to_owned(),
            delivery_note: None,
        },
        quoted_fee: None,
        ready_at: fixtures::instant(),
    }
}

/// A booking comes back with the courier's reference.
///
/// # Errors
///
/// [`CaseFailure`] if the obligation does not hold.
pub async fn books_a_courier<H: ShippingDispatchHarness>(harness: &H) -> Result<(), CaseFailure> {
    let courier = harness.fresh().await?;
    let shipment = courier
        .create_delivery(&request(harness.store_id(), 1))
        .await?;
    let obligation = booking();
    obligation.require(
        !shipment.courier_job_ref.as_str().is_empty(),
        "a booking returns a courier reference, or nothing can track or cancel it",
    )?;
    obligation.require(
        !ShipmentUpdate {
            shipment_id: shipment.shipment_id,
            courier_job_ref: shipment.courier_job_ref.clone(),
            status: shipment.status.clone(),
            at: shipment.updated_at,
        }
        .is_terminal(),
        "and a fresh booking is not already finished",
    )
}

/// Retrying does not summon a second motorbike.
///
/// # Errors
///
/// [`CaseFailure`] if the obligation does not hold.
pub async fn is_idempotent_by_shipment_id<H: ShippingDispatchHarness>(
    harness: &H,
) -> Result<(), CaseFailure> {
    let courier = harness.fresh().await?;
    let request = request(harness.store_id(), 1);
    let first = courier.create_delivery(&request).await?;
    let second = courier.create_delivery(&request).await?;
    booking().require_eq(
        &second.courier_job_ref,
        &first.courier_job_ref,
        "the same shipment identifier books one job — a retry after a timeout must not put two \
         riders on the same order, which the store then pays for twice",
    )
}

/// Polling a live job works.
///
/// # Errors
///
/// [`CaseFailure`] if the obligation does not hold.
pub async fn tracks_a_booked_job<H: ShippingDispatchHarness>(
    harness: &H,
) -> Result<(), CaseFailure> {
    let courier = harness.fresh().await?;
    let booked = courier
        .create_delivery(&request(harness.store_id(), 1))
        .await?;
    let tracked = courier.track(&booked.courier_job_ref).await?;
    tracking().require_eq(
        &tracked.shipment_id,
        &booked.shipment_id,
        "tracking returns the job that was asked about",
    )
}

/// Cancelling a live job works.
///
/// # Errors
///
/// [`CaseFailure`] if the obligation does not hold.
pub async fn cancels_a_booked_job<H: ShippingDispatchHarness>(
    harness: &H,
) -> Result<(), CaseFailure> {
    let courier = harness.fresh().await?;
    let booked = courier
        .create_delivery(&request(harness.store_id(), 1))
        .await?;
    let cancelled = courier.cancel(&booked.courier_job_ref).await?;
    cancellation().require_eq(
        &cancelled.status.known(),
        &ShipmentStatus::Cancelled,
        "a cancelled job reports itself cancelled",
    )
}

/// Cancellation is retried, so repeats must succeed.
///
/// # Errors
///
/// [`CaseFailure`] if the obligation does not hold.
pub async fn cancels_idempotently<H: ShippingDispatchHarness>(
    harness: &H,
) -> Result<(), CaseFailure> {
    let courier = harness.fresh().await?;
    let booked = courier
        .create_delivery(&request(harness.store_id(), 1))
        .await?;
    courier.cancel(&booked.courier_job_ref).await?;
    cancellation().require(
        courier.cancel(&booked.courier_job_ref).await.is_ok(),
        "cancelling an already-cancelled job succeeds",
    )
}

/// A delivered job cannot be un-delivered.
///
/// # Errors
///
/// [`CaseFailure`] if the obligation does not hold.
pub async fn refuses_to_cancel_a_completed_job<H: ShippingDispatchHarness>(
    harness: &H,
) -> Result<(), CaseFailure> {
    let courier = harness.fresh().await?;
    let booked = courier
        .create_delivery(&request(harness.store_id(), 1))
        .await?;
    harness.complete(&courier, &booked.courier_job_ref).await?;
    cancellation().require_error(
        courier.cancel(&booked.courier_job_ref).await,
        ErrorStatus::FailedPrecondition,
        "cancelling a completed job must fail rather than succeed quietly. A successful-looking \
         cancel leaves the store expecting a refund it is not going to get",
    )
}

/// A finished job is still trackable, because callbacks get missed.
///
/// # Errors
///
/// [`CaseFailure`] if the obligation does not hold.
pub async fn tracks_a_completed_job_for_reconciliation<H: ShippingDispatchHarness>(
    harness: &H,
) -> Result<(), CaseFailure> {
    let courier = harness.fresh().await?;
    let booked = courier
        .create_delivery(&request(harness.store_id(), 1))
        .await?;
    harness.complete(&courier, &booked.courier_job_ref).await?;

    let tracked = courier.track(&booked.courier_job_ref).await?;
    tracking().require_eq(
        &tracked.status.known(),
        &ShipmentStatus::Completed,
        "a finished job still answers a poll. A courier's callback lands on the cloud's HTTP \
         surface and can be missed, and this is how the missed one is recovered",
    )
}

/// An unknown reference is not found.
///
/// # Errors
///
/// [`CaseFailure`] if the obligation does not hold.
pub async fn reports_an_unknown_job_as_not_found<H: ShippingDispatchHarness>(
    harness: &H,
) -> Result<(), CaseFailure> {
    let courier = harness.fresh().await?;
    tracking().require_error(
        courier.track(&CourierJobRef::new("never-booked")).await,
        ErrorStatus::NotFound,
        "an unknown courier reference is not_found rather than an empty success",
    )
}
